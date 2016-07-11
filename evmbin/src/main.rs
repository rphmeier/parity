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

//! Parity EVM interpreter binary.

#![warn(missing_docs)]
extern crate ethcore;
extern crate rustc_serialize;
extern crate docopt;
#[macro_use]
extern crate ethcore_util as util;

mod ext;

use std::time::Instant;
use std::str::FromStr;
use docopt::Docopt;
use util::{U256, FromHex, Uint, Bytes};
use ethcore::evm::{Factory, VMType, Finalize};
use ethcore::action_params::ActionParams;

const USAGE: &'static str = r#"
EVM implementation for Parity.
  Copyright 2016 Ethcore (UK) Limited

Usage:
    evmbin stats [options]
    evmbin [-h | --help]

Transaction options:
    --code CODE        Contract code.
    --input DATA       Input data.
    --gas GAS          Supplied gas.

General options:
    -h, --help         Display this message and exit.
"#;


fn main() {
	let args: Args = Docopt::new(USAGE).and_then(|d| d.decode()).unwrap_or_else(|e| e.exit());

	let mut params = ActionParams::default();
	params.gas = args.gas();
	params.code = Some(args.code());
	params.data = args.data();

	let factory = Factory::new(VMType::Interpreter);
	let mut vm = factory.create(params.gas);
	let mut ext = ext::FakeExt::default();

	let start = Instant::now();
	let gas_left = vm.exec(params, &mut ext).finalize(ext).expect("OK");
	let duration = start.elapsed();

	println!("Gas used: {:?}", args.gas() - gas_left);
	println!("Output: {:?}", "");
	println!("Time: {}.{:.9}s", duration.as_secs(), duration.subsec_nanos());
}

#[derive(Debug, RustcDecodable)]
struct Args {
	cmd_stats: bool,
	flag_code: Option<String>,
	flag_gas: Option<String>,
	flag_input: Option<String>,
}

impl Args {
	pub fn gas(&self) -> U256 {
		self.flag_gas
			.clone()
			.and_then(|g| U256::from_str(&g).ok())
			.unwrap_or_else(|| !U256::zero())
	}

	pub fn code(&self) -> Bytes {
		self.flag_code
			.clone()
			.and_then(|c| c.from_hex().ok())
			.unwrap_or_else(|| die("Code is required."))
	}

	pub fn data(&self) -> Option<Bytes> {
		self.flag_input
			.clone()
			.and_then(|d| d.from_hex().ok())
	}
}


fn die(msg: &'static str) -> ! {
	println!("{}", msg);
	::std::process::exit(-1)
}
