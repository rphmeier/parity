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

use blockchain::BlockProvider;
use engine::Engine;
use error::Error;
use header::Header;
use super::Verifier;

#[allow(dead_code)]
pub struct NoopVerifier;

impl Verifier for NoopVerifier {
	fn verify_block_family(&self, _header: &Header, _bytes: &[u8], _engine: &Engine, _bc: &BlockProvider) -> Result<(), Error> {
		Ok(())
	}

	fn verify_block_final(&self, _expected: &Header, _got: &Header) -> Result<(), Error> {
		Ok(())
	}
}
