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

use std::io::Write;
use hyper::header;
use hyper::server;
use hyper::uri::RequestUri;
use hyper::net::HttpStream;
use hyper::status::StatusCode;
use hyper::{Decoder, Encoder, Next};
use endpoint::EndpointPath;

/// Represents a file that can be sent to client.
/// Implementation should keep track of bytes already sent internally.
pub trait DappFile: Send {
	/// Returns a content-type of this file.
	fn content_type(&self) -> &str;

	/// Checks if all bytes from that file were written.
	fn is_drained(&self) -> bool;

	/// Fetch next chunk to write to the client.
	fn next_chunk(&mut self) -> &[u8];

	/// How many files have been written to the client.
	fn bytes_written(&mut self, bytes: usize);
}

/// Dapp as a (dynamic) set of files.
pub trait Dapp: Send + 'static {
	/// File type
	type DappFile: DappFile;

	/// Returns file under given path.
	fn file(&self, path: &str) -> Option<Self::DappFile>;
}

/// A handler for a single webapp.
/// Resolves correct paths and serves as a plumbing code between
/// hyper server and dapp.
pub struct PageHandler<T: Dapp> {
	/// A Dapp.
	pub app: T,
	/// File currently being served (or `None` if file does not exist).
	pub file: Option<T::DappFile>,
	/// Optional prefix to strip from path.
	pub prefix: Option<String>,
	/// Requested path.
	pub path: EndpointPath,
	/// Flag indicating if the file can be safely embeded (put in iframe).
	pub safe_to_embed: bool,
}

impl<T: Dapp> PageHandler<T> {
	fn extract_path(&self, path: &str) -> String {
		let app_id = &self.path.app_id;
		let prefix = "/".to_owned() + self.prefix.as_ref().unwrap_or(app_id);
		let prefix_with_slash = prefix.clone() + "/";
		let query_pos = path.find('?').unwrap_or_else(|| path.len());

		// Index file support
		match path == "/" || path == &prefix || path == &prefix_with_slash {
			true => "index.html".to_owned(),
			false => if path.starts_with(&prefix_with_slash) {
				path[prefix_with_slash.len()..query_pos].to_owned()
			} else if path.starts_with("/") {
				path[1..query_pos].to_owned()
			} else {
				path[0..query_pos].to_owned()
			}
		}
	}
}

impl<T: Dapp> server::Handler<HttpStream> for PageHandler<T> {
	fn on_request(&mut self, req: server::Request<HttpStream>) -> Next {
		self.file = match *req.uri() {
			RequestUri::AbsolutePath(ref path) => {
				self.app.file(&self.extract_path(path))
			},
			RequestUri::AbsoluteUri(ref url) => {
				self.app.file(&self.extract_path(url.path()))
			},
			_ => None,
		};
		Next::write()
	}

	fn on_request_readable(&mut self, _decoder: &mut Decoder<HttpStream>) -> Next {
		Next::write()
	}

	fn on_response(&mut self, res: &mut server::Response) -> Next {
		if let Some(ref f) = self.file {
			res.set_status(StatusCode::Ok);
			res.headers_mut().set(header::ContentType(f.content_type().parse().unwrap()));
			if !self.safe_to_embed {
				res.headers_mut().set_raw("X-Frame-Options", vec![b"SAMEORIGIN".to_vec()]);
			}
			Next::write()
		} else {
			res.set_status(StatusCode::NotFound);
			Next::write()
		}
	}

	fn on_response_writable(&mut self, encoder: &mut Encoder<HttpStream>) -> Next {
		match self.file {
			None => Next::end(),
			Some(ref f) if f.is_drained() => Next::end(),
			Some(ref mut f) => match encoder.write(f.next_chunk()) {
				Ok(bytes) => {
					f.bytes_written(bytes);
					Next::write()
				},
				Err(e) => match e.kind() {
					::std::io::ErrorKind::WouldBlock => Next::write(),
					_ => Next::end(),
				},
			}
		}
	}
}



#[cfg(test)]
mod test {
	use super::*;

	pub struct TestWebAppFile;

	impl DappFile for TestWebAppFile {
		fn content_type(&self) -> &str {
			unimplemented!()
		}

		fn is_drained(&self) -> bool {
			unimplemented!()
		}

		fn next_chunk(&mut self) -> &[u8] {
			unimplemented!()
		}

		fn bytes_written(&mut self, _bytes: usize) {
			unimplemented!()
		}
	}

	#[derive(Default)]
	pub struct TestWebapp;

	impl Dapp for TestWebapp {
		type DappFile = TestWebAppFile;

		fn file(&self, _path: &str) -> Option<Self::DappFile> {
			None
		}
	}
}

#[test]
fn should_extract_path_with_appid() {

	// given
	let path1 = "/";
	let path2= "/test.css";
	let path3 = "/app/myfile.txt";
	let path4 = "/app/myfile.txt?query=123";
	let page_handler = PageHandler {
		app: test::TestWebapp,
		prefix: None,
		path: EndpointPath {
			app_id: "app".to_owned(),
			host: "".to_owned(),
			port: 8080
		},
		file: None,
		safe_to_embed: true,
	};

	// when
	let res1 = page_handler.extract_path(path1);
	let res2 = page_handler.extract_path(path2);
	let res3 = page_handler.extract_path(path3);
	let res4 = page_handler.extract_path(path4);

	// then
	assert_eq!(&res1, "index.html");
	assert_eq!(&res2, "test.css");
	assert_eq!(&res3, "myfile.txt");
	assert_eq!(&res4, "myfile.txt");
}
