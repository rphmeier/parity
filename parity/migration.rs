// Copyright 2015, 2016 Ethcore (UK) Ltd.
// This file is part of Parity.

// Parity is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

// Parity is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.

// You should have received a copy of the GNU General Public License
// along with Parity.  If not, see <http://www.gnu.org/licenses/>.

use std::fs;
use std::fs::File;
use std::io::{Read, Write, Error as IoError, ErrorKind};
use std::path::{Path, PathBuf};
use std::fmt::{Display, Formatter, Error as FmtError};
use util::journaldb::Algorithm;
use util::migration::{Manager as MigrationManager, Config as MigrationConfig, Error as MigrationError};
use ethcore::migrations;

/// Database is assumed to be at default version, when no version file is found.
const DEFAULT_VERSION: u32 = 5;
/// Current version of database models.
const CURRENT_VERSION: u32 = 7;
/// Defines how many items are migrated to the new version of database at once.
const BATCH_SIZE: usize = 1024;
/// Version file name.
const VERSION_FILE_NAME: &'static str = "db_version";

/// Migration related erorrs.
#[derive(Debug)]
pub enum Error {
	/// Returned when current version cannot be read or guessed.
	UnknownDatabaseVersion,
	/// Returned when migration is not possible.
	MigrationImpossible,
	/// Returned when migration unexpectadly failed.
	MigrationFailed,
	/// Returned when migration was completed succesfully,
	/// but there was a problem with io.
	Io(IoError),
}

impl Display for Error {
	fn fmt(&self, f: &mut Formatter) -> Result<(), FmtError> {
		let out = match *self {
			Error::UnknownDatabaseVersion => "Current database version cannot be read".into(),
			Error::MigrationImpossible => format!("Migration to version {} is not possible", CURRENT_VERSION),
			Error::MigrationFailed => "Migration unexpectedly failed".into(),
			Error::Io(ref err) => format!("Unexpected io error: {}", err),
		};

		write!(f, "{}", out)
	}
}

impl From<IoError> for Error {
	fn from(err: IoError) -> Self {
		Error::Io(err)
	}
}

impl From<MigrationError> for Error {
	fn from(err: MigrationError) -> Self {
		match err {
			MigrationError::Io(e) => Error::Io(e),
			_ => Error::MigrationFailed,
		}
	}
}

/// Returns the version file path.
fn version_file_path(path: &Path) -> PathBuf {
	let mut file_path = path.to_owned();
	file_path.push(VERSION_FILE_NAME);
	file_path
}

/// Reads current database version from the file at given path.
/// If the file does not exist returns `DEFAULT_VERSION`.
fn current_version(path: &Path) -> Result<u32, Error> {
	match File::open(version_file_path(path)) {
		Err(ref err) if err.kind() == ErrorKind::NotFound => Ok(DEFAULT_VERSION),
		Err(_) => Err(Error::UnknownDatabaseVersion),
		Ok(mut file) => {
			let mut s = String::new();
			try!(file.read_to_string(&mut s).map_err(|_| Error::UnknownDatabaseVersion));
			u32::from_str_radix(&s, 10).map_err(|_| Error::UnknownDatabaseVersion)
		},
	}
}

/// Writes current database version to the file.
/// Creates a new file if the version file does not exist yet.
fn update_version(path: &Path) -> Result<(), Error> {
	try!(fs::create_dir_all(path));
	let mut file = try!(File::create(version_file_path(path)));
	try!(file.write_all(format!("{}", CURRENT_VERSION).as_bytes()));
	Ok(())
}

/// Blocks database path.
fn blocks_database_path(path: &Path) -> PathBuf {
	let mut blocks_path = path.to_owned();
	blocks_path.push("blocks");
	blocks_path
}

/// Extras database path.
fn extras_database_path(path: &Path) -> PathBuf {
	let mut extras_path = path.to_owned();
	extras_path.push("extras");
	extras_path
}

/// State database path.
fn state_database_path(path: &Path) -> PathBuf {
	let mut state_path = path.to_owned();
	state_path.push("state");
	state_path
}

/// Database backup
fn backup_database_path(path: &Path) -> PathBuf {
	let mut backup_path = path.to_owned();
	backup_path.pop();
	backup_path.push("temp_backup");
	backup_path
}

/// Default migration settings.
fn default_migration_settings() -> MigrationConfig {
	MigrationConfig {
		batch_size: BATCH_SIZE,
	}
}

/// Migrations on the blocks database.
fn blocks_database_migrations() -> Result<MigrationManager, Error> {
	let manager = MigrationManager::new(default_migration_settings());
	Ok(manager)
}

/// Migrations on the extras database.
fn extras_database_migrations() -> Result<MigrationManager, Error> {
	let mut manager = MigrationManager::new(default_migration_settings());
	try!(manager.add_migration(migrations::extras::ToV6).map_err(|_| Error::MigrationImpossible));
	Ok(manager)
}

/// Migrations on the state database.
fn state_database_migrations(pruning: Algorithm) -> Result<MigrationManager, Error> {
	let mut manager = MigrationManager::new(default_migration_settings());
	let res = match pruning {
		Algorithm::Archive => manager.add_migration(migrations::state::ArchiveV7::default()),
		Algorithm::OverlayRecent => manager.add_migration(migrations::state::OverlayRecentV7::default()),
		_ => die!("Unsupported pruning method for migration. Delete DB and resync"),
	};

	try!(res.map_err(|_| Error::MigrationImpossible));
	Ok(manager)
}

/// Migrates database at given position with given migration rules.
fn migrate_database(version: u32, db_path: PathBuf, mut migrations: MigrationManager) -> Result<(), Error> {
	// check if migration is needed
	if !migrations.is_needed(version) {
		return Ok(())
	}

	let backup_path = backup_database_path(&db_path);
	// remove the backup dir if it exists
	let _ = fs::remove_dir_all(&backup_path);

	// migrate old database to the new one
	let temp_path = try!(migrations.execute(&db_path, version));

	// create backup
	try!(fs::rename(&db_path, &backup_path));

	// replace the old database with the new one
	if let Err(err) = fs::rename(&temp_path, &db_path) {
		// if something went wrong, bring back backup
		try!(fs::rename(&backup_path, &db_path));
		return Err(err.into());
	}

	// remove backup
	try!(fs::remove_dir_all(&backup_path));

	Ok(())
}

fn exists(path: &Path) -> bool {
	fs::metadata(path).is_ok()
}

/// Migrates the database.
pub fn migrate(path: &Path, pruning: Algorithm) -> Result<(), Error> {
	// read version file.
	let version = try!(current_version(path));

	// migrate the databases.
	// main db directory may already exists, so let's check if we have blocks dir
	if version != CURRENT_VERSION && exists(&blocks_database_path(path)) {
		println!("Migrating database from version {} to {}", version, CURRENT_VERSION);
		try!(migrate_database(version, blocks_database_path(path), try!(blocks_database_migrations())));
		try!(migrate_database(version, extras_database_path(path), try!(extras_database_migrations())));
		try!(migrate_database(version, state_database_path(path), try!(state_database_migrations(pruning))));
		println!("Migration finished");
	}

	// update version file.
	update_version(path)
}

