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

//! Fetchable Dapps support.
//! Manages downloaded (cached) Dapps and downloads them when necessary.
//! Uses `URLHint` to resolve addresses into Dapps bundle file location.

use zip;
use std::{fs, env};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::collections::HashMap;
use rustc_serialize::hex::FromHex;

use hyper::Control;
use hyper::status::StatusCode;

use random_filename;
use util::{Mutex, H256};
use util::sha3::sha3;
use page::LocalPageEndpoint;
use handlers::{ContentHandler, AppFetcherHandler, DappHandler};
use endpoint::{Endpoint, EndpointPath, Handler};
use apps::manifest::{MANIFEST_FILENAME, deserialize_manifest, serialize_manifest, Manifest};
use apps::urlhint::{URLHintContract, URLHint};

enum AppStatus {
	Fetching,
	Ready(LocalPageEndpoint),
}

pub struct AppFetcher<R: URLHint = URLHintContract> {
	dapps_path: PathBuf,
	resolver: R,
	dapps: Arc<Mutex<HashMap<String, AppStatus>>>,
}

impl<R: URLHint> Drop for AppFetcher<R> {
	fn drop(&mut self) {
		// Clear cache path
		let _ = fs::remove_dir_all(&self.dapps_path);
	}
}

impl<R: URLHint> AppFetcher<R> {

	pub fn new(resolver: R) -> Self {
		let mut dapps_path = env::temp_dir();
		dapps_path.push(random_filename());

		AppFetcher {
			dapps_path: dapps_path,
			resolver: resolver,
			dapps: Arc::new(Mutex::new(HashMap::new())),
		}
	}

	#[cfg(test)]
	fn set_status(&self, app_id: &str, status: AppStatus) {
		self.dapps.lock().insert(app_id.to_owned(), status);
	}

	pub fn contains(&self, app_id: &str) -> bool {
		let dapps = self.dapps.lock();
		match dapps.get(app_id) {
			// Check if we already have the app
			Some(_) => true,
			// fallback to resolver
			None => match app_id.from_hex() {
				Ok(app_id) => self.resolver.resolve(app_id).is_some(),
				_ => false,
			},
		}
	}

	pub fn to_handler(&self, path: EndpointPath, control: Control) -> Box<Handler> {
		let mut dapps = self.dapps.lock();
		let app_id = path.app_id.clone();

		let (new_status, handler) = {
			let status = dapps.get(&app_id);
			match status {
				// Just server dapp
				Some(&AppStatus::Ready(ref endpoint)) => {
					(None, endpoint.to_handler(path))
				},
				// App is already being fetched
				Some(&AppStatus::Fetching) => {
					(None, Box::new(ContentHandler::html(
						StatusCode::ServiceUnavailable,
						format!(
							"<html><head>{}</head><body>{}</body></html>",
							"<meta http-equiv=\"refresh\" content=\"1\">",
							"<h1>This dapp is already being downloaded.</h1><h2>Please wait...</h2>",
						)
					)) as Box<Handler>)
				},
				// We need to start fetching app
				None => {
					// TODO [todr] Keep only last N dapps available!
					let app_hex = app_id.from_hex().expect("to_handler is called only when `contains` returns true.");
					let app = self.resolver.resolve(app_hex).expect("to_handler is called only when `contains` returns true.");
					(Some(AppStatus::Fetching), Box::new(AppFetcherHandler::new(
						app,
						control,
						path.using_dapps_domains,
						DappInstaller {
							dapp_id: app_id.clone(),
							dapps_path: self.dapps_path.clone(),
							dapps: self.dapps.clone(),
						}
					)) as Box<Handler>)
				},
			}
		};

		if let Some(status) = new_status {
			dapps.insert(app_id, status);
		}

		handler
	}
}

#[derive(Debug)]
pub enum ValidationError {
	Io(io::Error),
	Zip(zip::result::ZipError),
	InvalidDappId,
	ManifestNotFound,
	ManifestSerialization(String),
	HashMismatch { expected: H256, got: H256, },
}

impl From<io::Error> for ValidationError {
	fn from(err: io::Error) -> Self {
		ValidationError::Io(err)
	}
}

impl From<zip::result::ZipError> for ValidationError {
	fn from(err: zip::result::ZipError) -> Self {
		ValidationError::Zip(err)
	}
}

struct DappInstaller {
	dapp_id: String,
	dapps_path: PathBuf,
	dapps: Arc<Mutex<HashMap<String, AppStatus>>>,
}

impl DappInstaller {
	fn find_manifest(zip: &mut zip::ZipArchive<fs::File>) -> Result<(Manifest, PathBuf), ValidationError> {
		for i in 0..zip.len() {
			let mut file = try!(zip.by_index(i));

			if !file.name().ends_with(MANIFEST_FILENAME) {
				continue;
			}

			// try to read manifest
			let mut manifest = String::new();
			let manifest = file
				.read_to_string(&mut manifest).ok()
				.and_then(|_| deserialize_manifest(manifest).ok());

			if let Some(manifest) = manifest {
				let mut manifest_location = PathBuf::from(file.name());
				manifest_location.pop(); // get rid of filename
				return Ok((manifest, manifest_location));
			}
		}

		Err(ValidationError::ManifestNotFound)
	}

	fn dapp_target_path(&self, manifest: &Manifest) -> PathBuf {
		let mut target = self.dapps_path.clone();
		target.push(&manifest.id);
		target
	}
}

impl DappHandler for DappInstaller {
	type Error = ValidationError;

	fn validate_and_install(&self, app_path: PathBuf) -> Result<Manifest, ValidationError> {
		trace!(target: "dapps", "Opening dapp bundle at {:?}", app_path);
		let mut file = try!(fs::File::open(app_path));
		let hash = try!(sha3(&mut file));
		let dapp_id = try!(self.dapp_id.as_str().parse().map_err(|_| ValidationError::InvalidDappId));
		if dapp_id != hash {
			return Err(ValidationError::HashMismatch {
				expected: dapp_id,
				got: hash,
			});
		}
		// Unpack archive
		let mut zip = try!(zip::ZipArchive::new(file));
		// First find manifest file
		let (mut manifest, manifest_dir) = try!(Self::find_manifest(&mut zip));
		// Overwrite id to match hash
		manifest.id = self.dapp_id.clone();

		let target = self.dapp_target_path(&manifest);

		// Remove old directory
		if target.exists() {
			warn!(target: "dapps", "Overwriting existing dapp: {}", manifest.id);
			try!(fs::remove_dir_all(target.clone()));
		}

		// Unpack zip
		for i in 0..zip.len() {
			let mut file = try!(zip.by_index(i));
			// TODO [todr] Check if it's consistent on windows.
			let is_dir = file.name().chars().rev().next() == Some('/');

			let file_path = PathBuf::from(file.name());
			let location_in_manifest_base = file_path.strip_prefix(&manifest_dir);
			// Create files that are inside manifest directory
			if let Ok(location_in_manifest_base) = location_in_manifest_base {
				let p = target.join(location_in_manifest_base);
				// Check if it's a directory
				if is_dir {
					try!(fs::create_dir_all(p));
				} else {
					let mut target = try!(fs::File::create(p));
					try!(io::copy(&mut file, &mut target));
				}
			}
		}

		// Write manifest
		let manifest_str = try!(serialize_manifest(&manifest).map_err(ValidationError::ManifestSerialization));
		let manifest_path = target.join(MANIFEST_FILENAME);
		let mut manifest_file = try!(fs::File::create(manifest_path));
		try!(manifest_file.write_all(manifest_str.as_bytes()));

		// Return modified app manifest
		Ok(manifest)
	}

	fn done(&self, manifest: Option<&Manifest>) {
		let mut dapps = self.dapps.lock();
		match manifest {
			Some(manifest) => {
				let path = self.dapp_target_path(manifest);
				let app = LocalPageEndpoint::new(path, manifest.clone().into());
				dapps.insert(self.dapp_id.clone(), AppStatus::Ready(app));
			},
			// In case of error
			None => {
				dapps.remove(&self.dapp_id);
			},
		}
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use super::{AppFetcher, AppStatus};
	use apps::urlhint::{GithubApp, URLHint};
	use endpoint::EndpointInfo;
	use page::LocalPageEndpoint;
	use util::Bytes;

	struct FakeResolver;
	impl URLHint for FakeResolver {
		fn resolve(&self, _app_id: Bytes) -> Option<GithubApp> {
			None
		}
	}

	#[test]
	fn should_true_if_contains_the_app() {
		// given
		let fetcher = AppFetcher::new(FakeResolver);
		let handler = LocalPageEndpoint::new(PathBuf::from("/tmp/test"), EndpointInfo {
			name: "fake".into(),
			description: "".into(),
			version: "".into(),
			author: "".into(),
			icon_url: "".into(),
		});

		// when
		fetcher.set_status("test", AppStatus::Ready(handler));
		fetcher.set_status("test2", AppStatus::Fetching);

		// then
		assert_eq!(fetcher.contains("test"), true);
		assert_eq!(fetcher.contains("test2"), true);
		assert_eq!(fetcher.contains("test3"), false);
	}
}

