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

use client::{BlockChainClient, Client, ClientConfig};
use common::*;
use spec::*;
use block::{OpenBlock};
use blockchain::{BlockChain, Config as BlockChainConfig};
use state::*;
use evm::Schedule;
use engine::*;
use ethereum;
use devtools::*;
use miner::Miner;

#[cfg(feature = "json-tests")]
pub enum ChainEra {
	Frontier,
	Homestead,
}

#[cfg(test)]
pub struct GuardedTempResult<T> {
	result: Option<T>,
	_temp: RandomTempPath
}

impl<T> GuardedTempResult<T> {
    pub fn reference(&self) -> &T {
        self.result.as_ref().unwrap()
    }

    pub fn reference_mut(&mut self) -> &mut T {
    	self.result.as_mut().unwrap()
    }

	pub fn take(&mut self) -> T {
		self.result.take().unwrap()
	}
}

pub struct TestEngine {
	engine: Box<Engine>,
	max_depth: usize
}

impl TestEngine {
	pub fn new(max_depth: usize) -> TestEngine {
		TestEngine {
			engine: ethereum::new_frontier_test().engine,
			max_depth: max_depth
		}
	}
}

impl Engine for TestEngine {
	fn name(&self) -> &str {
		"TestEngine"
	}

	fn params(&self) -> &CommonParams {
		self.engine.params()
	}

	fn builtins(&self) -> &BTreeMap<Address, Builtin> {
		self.engine.builtins()
	}

	fn schedule(&self, _env_info: &EnvInfo) -> Schedule {
		let mut schedule = Schedule::new_frontier();
		schedule.max_depth = self.max_depth;
		schedule
	}
}

// TODO: move everything over to get_null_spec.
pub fn get_test_spec() -> Spec {
	Spec::new_test()
}

pub fn create_test_block(header: &Header) -> Bytes {
	let mut rlp = RlpStream::new_list(3);
	rlp.append(header);
	rlp.append_raw(&rlp::EMPTY_LIST_RLP, 1);
	rlp.append_raw(&rlp::EMPTY_LIST_RLP, 1);
	rlp.out()
}

fn create_unverifiable_block_header(order: u32, parent_hash: H256) -> Header {
	let mut header = Header::new();
	header.gas_limit = 0.into();
	header.difficulty = (order * 100).into();
	header.timestamp = (order * 10) as u64;
	header.number = order as u64;
	header.parent_hash = parent_hash;
	header.state_root = H256::zero();

	header
}

fn create_unverifiable_block_with_extra(order: u32, parent_hash: H256, extra: Option<Bytes>) -> Bytes {
	let mut header = create_unverifiable_block_header(order, parent_hash);
	header.extra_data = match extra {
		Some(extra_data) => extra_data,
		None => {
			let base = (order & 0x000000ff) as u8;
			let generated: Vec<u8> = vec![base + 1, base + 2, base + 3];
			generated
		}
	};
	create_test_block(&header)
}

fn create_unverifiable_block(order: u32, parent_hash: H256) -> Bytes {
	create_test_block(&create_unverifiable_block_header(order, parent_hash))
}

pub fn create_test_block_with_data(header: &Header, transactions: &[SignedTransaction], uncles: &[Header]) -> Bytes {
	let mut rlp = RlpStream::new_list(3);
	rlp.append(header);
	rlp.begin_list(transactions.len());
	for t in transactions {
		rlp.append_raw(&encode::<SignedTransaction>(t).to_vec(), 1);
	}
	rlp.append(&uncles);
	rlp.out()
}

pub fn generate_dummy_client(block_number: u32) -> GuardedTempResult<Arc<Client>> {
	generate_dummy_client_with_spec_and_data(Spec::new_test, block_number, 0, &[])
}

pub fn generate_dummy_client_with_data(block_number: u32, txs_per_block: usize, tx_gas_prices: &[U256]) -> GuardedTempResult<Arc<Client>> {
	generate_dummy_client_with_spec_and_data(Spec::new_null, block_number, txs_per_block, tx_gas_prices)
}

pub fn generate_dummy_client_with_spec_and_data<F>(get_test_spec: F, block_number: u32, txs_per_block: usize, tx_gas_prices: &[U256]) -> GuardedTempResult<Arc<Client>> where F: Fn()->Spec {
	let dir = RandomTempPath::new();

	let test_spec = get_test_spec();
	let client = Client::new(ClientConfig::default(), get_test_spec(), dir.as_path(), Arc::new(Miner::with_spec(get_test_spec())), IoChannel::disconnected()).unwrap();
	let test_engine = &test_spec.engine;

	let mut db_result = get_temp_journal_db();
	let mut db = db_result.take();
	test_spec.ensure_db_good(db.as_hashdb_mut());
	let vm_factory = Default::default();
	let genesis_header = test_spec.genesis_header();

	let mut rolling_timestamp = 40;
	let mut last_hashes = vec![];
	let mut last_header = genesis_header.clone();

	let kp = KeyPair::from_secret("".sha3()).unwrap()	;
	let author = kp.address();

	let mut n = 0;
	for _ in 0..block_number {
		last_hashes.push(last_header.hash());

		// forge block.
		let mut b = OpenBlock::new(
			test_engine.deref(),
			&vm_factory,
			false,
			db,
			&last_header,
			last_hashes.clone(),
			None,
			author.clone(),
			(3141562.into(), 31415620.into()),
			vec![]
		).unwrap();
		b.set_difficulty(U256::from(0x20000));
		rolling_timestamp += 10;
		b.set_timestamp(rolling_timestamp);

		// first block we don't have any balance, so can't send any transactions.
		for _ in 0..txs_per_block {
			b.push_transaction(Transaction {
				nonce: n.into(),
				gas_price: tx_gas_prices[n % tx_gas_prices.len()],
				gas: 100000.into(),
				action: Action::Create,
				data: vec![],
				value: U256::zero(),
			}.sign(kp.secret()), None).unwrap();
			n += 1;
		}

		let b = b.close_and_lock().seal(test_engine.deref(), vec![]).unwrap();

		if let Err(e) = client.import_block(b.rlp_bytes()) {
			panic!("error importing block which is valid by definition: {:?}", e);
		}

		last_header = BlockView::new(&b.rlp_bytes()).header();
		db = b.drain();
	}
	client.flush_queue();
	client.import_verified_blocks(&IoChannel::disconnected());

	GuardedTempResult::<Arc<Client>> {
		_temp: dir,
		result: Some(client)
	}
}

pub fn push_blocks_to_client(client: &Arc<Client>, timestamp_salt: u64, starting_number: usize, block_number: usize) {
	let test_spec = get_test_spec();
	let test_engine = &test_spec.engine;
	//let test_engine = test_spec.to_engine().unwrap();
	let state_root = test_spec.genesis_header().state_root;
	let mut rolling_hash = client.chain_info().best_block_hash;
	let mut rolling_block_number = starting_number as u64;
	let mut rolling_timestamp = timestamp_salt + starting_number as u64 * 10;

	for _ in 0..block_number {
		let mut header = Header::new();

		header.gas_limit = test_engine.params().min_gas_limit;
		header.difficulty = U256::from(0x20000);
		header.timestamp = rolling_timestamp;
		header.number = rolling_block_number;
		header.parent_hash = rolling_hash;
		header.state_root = state_root.clone();

		rolling_hash = header.hash();
		rolling_block_number = rolling_block_number + 1;
		rolling_timestamp = rolling_timestamp + 10;

		if let Err(e) = client.import_block(create_test_block(&header)) {
			panic!("error importing block which is valid by definition: {:?}", e);
		}
	}
}

pub fn get_test_client_with_blocks(blocks: Vec<Bytes>) -> GuardedTempResult<Arc<Client>> {
	let dir = RandomTempPath::new();
	let client = Client::new(ClientConfig::default(), get_test_spec(), dir.as_path(), Arc::new(Miner::with_spec(get_test_spec())), IoChannel::disconnected()).unwrap();
	for block in &blocks {
		if let Err(_) = client.import_block(block.clone()) {
			panic!("panic importing block which is well-formed");
		}
	}
	client.flush_queue();
	client.import_verified_blocks(&IoChannel::disconnected());

	GuardedTempResult::<Arc<Client>> {
		_temp: dir,
		result: Some(client)
	}
}

pub fn generate_dummy_blockchain(block_number: u32) -> GuardedTempResult<BlockChain> {
	let temp = RandomTempPath::new();
	let bc = BlockChain::new(BlockChainConfig::default(), &create_unverifiable_block(0, H256::zero()), temp.as_path());
	for block_order in 1..block_number {
		bc.insert_block(&create_unverifiable_block(block_order, bc.best_block_hash()), vec![]);
	}

	GuardedTempResult::<BlockChain> {
		_temp: temp,
		result: Some(bc)
	}
}

pub fn generate_dummy_blockchain_with_extra(block_number: u32) -> GuardedTempResult<BlockChain> {
	let temp = RandomTempPath::new();
	let bc = BlockChain::new(BlockChainConfig::default(), &create_unverifiable_block(0, H256::zero()), temp.as_path());
	for block_order in 1..block_number {
		bc.insert_block(&create_unverifiable_block_with_extra(block_order, bc.best_block_hash(), None), vec![]);
	}

	GuardedTempResult::<BlockChain> {
		_temp: temp,
		result: Some(bc)
	}
}

pub fn generate_dummy_empty_blockchain() -> GuardedTempResult<BlockChain> {
	let temp = RandomTempPath::new();
	let bc = BlockChain::new(BlockChainConfig::default(), &create_unverifiable_block(0, H256::zero()), temp.as_path());

	GuardedTempResult::<BlockChain> {
		_temp: temp,
		result: Some(bc)
	}
}

pub fn get_temp_journal_db() -> GuardedTempResult<Box<JournalDB>> {
	let temp = RandomTempPath::new();
	let journal_db = journaldb::new(temp.as_str(), journaldb::Algorithm::EarlyMerge, DatabaseConfig::default());
	GuardedTempResult {
		_temp: temp,
		result: Some(journal_db)
	}
}

pub fn get_temp_state() -> GuardedTempResult<State> {
	let temp = RandomTempPath::new();
	let journal_db = get_temp_journal_db_in(temp.as_path());
	GuardedTempResult {
	    _temp: temp,
		result: Some(State::new(journal_db, U256::from(0u8)))
	}
}

pub fn get_temp_journal_db_in(path: &Path) -> Box<JournalDB> {
	journaldb::new(path.to_str().unwrap(), journaldb::Algorithm::EarlyMerge, DatabaseConfig::default())
}

pub fn get_temp_state_in(path: &Path) -> State {
	let journal_db = get_temp_journal_db_in(path);
	State::new(journal_db, U256::from(0u8))
}

pub fn get_good_dummy_block_seq(count: usize) -> Vec<Bytes> {
	let test_spec = get_test_spec();
  	get_good_dummy_block_fork_seq(1, count, &test_spec.genesis_header().hash())
}

pub fn get_good_dummy_block_fork_seq(start_number: usize, count: usize, parent_hash: &H256) -> Vec<Bytes> {
	let test_spec = get_test_spec();
	let test_engine = &test_spec.engine;
	let mut rolling_timestamp = start_number as u64 * 10;
	let mut parent = *parent_hash;
	let mut r = Vec::new();
	for i in start_number .. start_number + count + 1 {
		let mut block_header = Header::new();
		block_header.gas_limit = test_engine.params().min_gas_limit;
		block_header.difficulty = U256::from(i).mul(U256([0, 1, 0, 0]));
		block_header.timestamp = rolling_timestamp;
		block_header.number = i as u64;
		block_header.parent_hash = parent;
		block_header.state_root = test_spec.genesis_header().state_root;

		parent = block_header.hash();
		rolling_timestamp = rolling_timestamp + 10;

		r.push(create_test_block(&block_header));

	}
	r
}

pub fn get_good_dummy_block() -> Bytes {
	let mut block_header = Header::new();
	let test_spec = get_test_spec();
	let test_engine = &test_spec.engine;
	block_header.gas_limit = test_engine.params().min_gas_limit;
	block_header.difficulty = U256::from(0x20000);
	block_header.timestamp = 40;
	block_header.number = 1;
	block_header.parent_hash = test_spec.genesis_header().hash();
	block_header.state_root = test_spec.genesis_header().state_root;

	create_test_block(&block_header)
}

pub fn get_bad_state_dummy_block() -> Bytes {
	let mut block_header = Header::new();
	let test_spec = get_test_spec();
	let test_engine = &test_spec.engine;
	block_header.gas_limit = test_engine.params().min_gas_limit;
	block_header.difficulty = U256::from(0x20000);
	block_header.timestamp = 40;
	block_header.number = 1;
	block_header.parent_hash = test_spec.genesis_header().hash();
	block_header.state_root = 0xbad.into();

	create_test_block(&block_header)
}
