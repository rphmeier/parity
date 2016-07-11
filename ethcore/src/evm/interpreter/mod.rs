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

//! Rust VM implementation

#[cfg(not(feature = "evm-debug"))]
macro_rules! evm_debug {
	($x: expr) => {}
}

#[cfg(feature = "evm-debug")]
macro_rules! evm_debug {
	($x: expr) => {
		$x
	}
}

mod gasometer;
mod stack;
mod memory;

use self::gasometer::Gasometer;
use self::stack::{Stack, VecStack};
use self::memory::Memory;

use std::marker::PhantomData;
use common::*;
use super::instructions::{self, Instruction, InstructionInfo};
use evm::{self, MessageCallResult, ContractCreateResult, GasLeft, CostType};

#[cfg(feature = "evm-debug")]
fn color(instruction: Instruction, name: &'static str) -> String {
	let c = instruction as usize % 6;
	let colors = [31, 34, 33, 32, 35, 36];
	format!("\x1B[1;{}m{}\x1B[0m", colors[c], name)
}

type CodePosition = usize;
type ProgramCounter = usize;

/// Abstraction over raw vector of Bytes. Easier state management of PC.
struct CodeReader<'a> {
	position: ProgramCounter,
	code: &'a Bytes
}

#[cfg_attr(feature="dev", allow(len_without_is_empty))]
impl<'a> CodeReader<'a> {
	/// Get `no_of_bytes` from code and convert to U256. Move PC
	fn read(&mut self, no_of_bytes: usize) -> U256 {
		let pos = self.position;
		self.position += no_of_bytes;
		let max = cmp::min(pos + no_of_bytes, self.code.len());
		U256::from(&self.code[pos..max])
	}

	fn len (&self) -> usize {
		self.code.len()
	}
}

enum InstructionResult<Gas> {
	Ok,
	UseAllGas,
	GasLeft(Gas),
	UnusedGas(Gas),
	JumpToPosition(U256),
	// gas left, init_orf, init_size
	StopExecutionNeedsReturn(Gas, U256, U256),
	StopExecution,
}


/// Intepreter EVM implementation
#[derive(Default)]
pub struct Interpreter<Cost: CostType> {
	mem: Vec<u8>,
	_type: PhantomData<Cost>,
}

impl<Cost: CostType> evm::Evm for Interpreter<Cost> {
	fn exec(&mut self, params: ActionParams, ext: &mut evm::Ext) -> evm::Result<GasLeft> {
		self.mem.clear();

		let code = &params.code.as_ref().unwrap();
		let valid_jump_destinations = self.find_jump_destinations(&code);

		let mut gasometer = Gasometer::<Cost>::new(try!(Cost::from_u256(params.gas)));
		let mut stack = VecStack::with_capacity(ext.schedule().stack_limit, U256::zero());
		let mut reader = CodeReader {
			position: 0,
			code: &code
		};

		while reader.position < code.len() {
			let instruction = code[reader.position];
			reader.position += 1;

			let info = instructions::get_info(instruction);
			try!(self.verify_instruction(ext, instruction, &info, &stack));

			// Calculate gas cost
			let (gas_cost, mem_size) = try!(gasometer.get_gas_cost_mem(ext, instruction, &info, &stack, self.mem.size()));
			// TODO: make compile-time removable if too much of a performance hit.
			let trace_executed = ext.trace_prepare_execute(reader.position - 1, instruction, &gas_cost.as_u256());

			try!(gasometer.verify_gas(&gas_cost));
			self.mem.expand(mem_size);
			gasometer.current_gas = gasometer.current_gas - gas_cost;

			evm_debug!({
				println!("[0x{:x}][{}(0x{:x}) Gas: {:x}\n  Gas Before: {:x}",
					reader.position,
					color(instruction, info.name),
					instruction,
					gas_cost,
					gasometer.current_gas + gas_cost
				);
			});

			let (mem_written, store_written) = match trace_executed {
				true => (Self::mem_written(instruction, &stack), Self::store_written(instruction, &stack)),
				false => (None, None),
			};

			// Execute instruction
			let result = try!(self.exec_instruction(
				gasometer.current_gas, &params, ext, instruction, &mut reader, &mut stack
			));

			if trace_executed {
				ext.trace_executed(gasometer.current_gas.as_u256(), stack.peek_top(info.ret), mem_written.map(|(o, s)| (o, &(self.mem[o..(o + s)]))), store_written);
			}

			// Advance
			match result {
				InstructionResult::Ok => {},
				InstructionResult::UnusedGas(gas) => {
					gasometer.current_gas = gasometer.current_gas + gas;
				},
				InstructionResult::UseAllGas => {
					gasometer.current_gas = Cost::from(0);
				},
				InstructionResult::GasLeft(gas_left) => {
					gasometer.current_gas = gas_left;
				},
				InstructionResult::JumpToPosition(position) => {
					let pos = try!(self.verify_jump(position, &valid_jump_destinations));
					reader.position = pos;
				},
				InstructionResult::StopExecutionNeedsReturn(gas, off, size) => {
					return Ok(GasLeft::NeedsReturn(gas.as_u256(), self.mem.read_slice(off, size)));
				},
				InstructionResult::StopExecution => break,
			}
		}

		Ok(GasLeft::Known(gasometer.current_gas.as_u256()))
	}
}

impl<Cost: CostType> Interpreter<Cost> {

	fn verify_instruction(&self, ext: &evm::Ext, instruction: Instruction, info: &InstructionInfo, stack: &Stack<U256>) -> evm::Result<()> {
		let schedule = ext.schedule();

		if !schedule.have_delegate_call && instruction == instructions::DELEGATECALL {
			return Err(evm::Error::BadInstruction {
				instruction: instruction
			});
		}

		if info.tier == instructions::GasPriceTier::Invalid {
			return Err(evm::Error::BadInstruction {
				instruction: instruction
			});
		}

		if !stack.has(info.args) {
			Err(evm::Error::StackUnderflow {
				instruction: info.name,
				wanted: info.args,
				on_stack: stack.size()
			})
		} else if stack.size() - info.args + info.ret > schedule.stack_limit {
			Err(evm::Error::OutOfStack {
				instruction: info.name,
				wanted: info.ret - info.args,
				limit: schedule.stack_limit
			})
		} else {
			Ok(())
		}
	}

	fn mem_written(
		instruction: Instruction,
		stack: &Stack<U256>
	) -> Option<(usize, usize)> {
		match instruction {
			instructions::MSTORE | instructions::MLOAD => Some((stack.peek(0).low_u64() as usize, 32)),
			instructions::MSTORE8 => Some((stack.peek(0).low_u64() as usize, 1)),
			instructions::CALLDATACOPY | instructions::CODECOPY => Some((stack.peek(0).low_u64() as usize, stack.peek(2).low_u64() as usize)),
			instructions::EXTCODECOPY => Some((stack.peek(1).low_u64() as usize, stack.peek(3).low_u64() as usize)),
			instructions::CALL | instructions::CALLCODE => Some((stack.peek(5).low_u64() as usize, stack.peek(6).low_u64() as usize)),
			instructions::DELEGATECALL => Some((stack.peek(4).low_u64() as usize, stack.peek(5).low_u64() as usize)),
			_ => None,
		}
	}

	fn store_written(
		instruction: Instruction,
		stack: &Stack<U256>
	) -> Option<(U256, U256)> {
		match instruction {
			instructions::SSTORE => Some((stack.peek(0).clone(), stack.peek(1).clone())),
			_ => None,
		}
	}

	#[cfg_attr(feature="dev", allow(too_many_arguments))]
	fn exec_instruction(
		&mut self,
		gas: Cost,
		params: &ActionParams,
		ext: &mut evm::Ext,
		instruction: Instruction,
		code: &mut CodeReader,
		stack: &mut Stack<U256>
	) -> evm::Result<InstructionResult<Cost>> {
		match instruction {
			instructions::JUMP => {
				let jump = stack.pop_back();
				return Ok(InstructionResult::JumpToPosition(
					jump
				));
			},
			instructions::JUMPI => {
				let jump = stack.pop_back();
				let condition = stack.pop_back();
				if !self.is_zero(&condition) {
					return Ok(InstructionResult::JumpToPosition(
						jump
					));
				}
			},
			instructions::JUMPDEST => {
				// ignore
			},
			instructions::CREATE => {
				let endowment = stack.pop_back();
				let init_off = stack.pop_back();
				let init_size = stack.pop_back();

				let contract_code = self.mem.read_slice(init_off, init_size);
				let can_create = ext.balance(&params.address) >= endowment && ext.depth() < ext.schedule().max_depth;

				if !can_create {
					stack.push(U256::zero());
					return Ok(InstructionResult::Ok);
				}

				let create_result = ext.create(&gas.as_u256(), &endowment, &contract_code);
				return match create_result {
					ContractCreateResult::Created(address, gas_left) => {
						stack.push(address_to_u256(address));
						Ok(InstructionResult::GasLeft(Cost::from_u256(gas_left).expect("Gas left cannot be greater.")))
					},
					ContractCreateResult::Failed => {
						stack.push(U256::zero());
						// TODO [todr] Should we just StopExecution here?
						Ok(InstructionResult::UseAllGas)
					}
				};
			},
			instructions::CALL | instructions::CALLCODE | instructions::DELEGATECALL => {
				assert!(ext.schedule().call_value_transfer_gas > ext.schedule().call_stipend, "overflow possible");
				let call_gas = Cost::from_u256(stack.pop_back()).expect("Gas is already validated.");
				let code_address = stack.pop_back();
				let code_address = u256_to_address(&code_address);

				let value = if instruction == instructions::DELEGATECALL {
					None
				} else {
					Some(stack.pop_back())
				};

				let in_off = stack.pop_back();
				let in_size = stack.pop_back();
				let out_off = stack.pop_back();
				let out_size = stack.pop_back();

				// Add stipend (only CALL|CALLCODE when value > 0)
				let call_gas = call_gas + value.map_or_else(|| Cost::from(0), |val| match val.is_zero() {
					false => Cost::from(ext.schedule().call_stipend),
					true => Cost::from(0)
				});

				// Get sender & receive addresses, check if we have balance
				let (sender_address, receive_address, has_balance) = match instruction {
					instructions::CALL => {
						let has_balance = ext.balance(&params.address) >= value.unwrap();
						(&params.address, &code_address, has_balance)
					},
					instructions::CALLCODE => {
						let has_balance = ext.balance(&params.address) >= value.unwrap();
						(&params.address, &params.address, has_balance)
					},
					instructions::DELEGATECALL => (&params.sender, &params.address, true),
					_ => panic!(format!("Unexpected instruction {} in CALL branch.", instruction))
				};

				let can_call = has_balance && ext.depth() < ext.schedule().max_depth;
				if !can_call {
					stack.push(U256::zero());
					return Ok(InstructionResult::UnusedGas(call_gas));
				}

				let call_result = {
					// we need to write and read from memory in the same time
					// and we don't want to copy
					let input = unsafe { ::std::mem::transmute(self.mem.read_slice(in_off, in_size)) };
					let output = self.mem.writeable_slice(out_off, out_size);
					ext.call(&call_gas.as_u256(), sender_address, receive_address, value, input, &code_address, output)
				};

				return match call_result {
					MessageCallResult::Success(gas_left) => {
						stack.push(U256::one());
						Ok(InstructionResult::UnusedGas(Cost::from_u256(gas_left).expect("Gas left cannot be greater then current one")))
					},
					MessageCallResult::Failed  => {
						stack.push(U256::zero());
						Ok(InstructionResult::Ok)
					}
				};
			},
			instructions::RETURN => {
				let init_off = stack.pop_back();
				let init_size = stack.pop_back();

				return Ok(InstructionResult::StopExecutionNeedsReturn(gas, init_off, init_size))
			},
			instructions::STOP => {
				return Ok(InstructionResult::StopExecution);
			},
			instructions::SUICIDE => {
				let address = stack.pop_back();
				ext.suicide(&u256_to_address(&address));
				return Ok(InstructionResult::StopExecution);
			},
			instructions::LOG0...instructions::LOG4 => {
				let no_of_topics = instructions::get_log_topics(instruction);

				let offset = stack.pop_back();
				let size = stack.pop_back();
				let topics = stack.pop_n(no_of_topics)
					.iter()
					.map(H256::from)
					.collect();
				ext.log(topics, self.mem.read_slice(offset, size));
			},
			instructions::PUSH1...instructions::PUSH32 => {
				let bytes = instructions::get_push_bytes(instruction);
				let val = code.read(bytes);
				stack.push(val);
			},
			instructions::MLOAD => {
				let word = self.mem.read(stack.pop_back());
				stack.push(U256::from(word));
			},
			instructions::MSTORE => {
				let offset = stack.pop_back();
				let word = stack.pop_back();
				Memory::write(&mut self.mem, offset, word);
			},
			instructions::MSTORE8 => {
				let offset = stack.pop_back();
				let byte = stack.pop_back();
				self.mem.write_byte(offset, byte);
			},
			instructions::MSIZE => {
				stack.push(U256::from(self.mem.size()));
			},
			instructions::SHA3 => {
				let offset = stack.pop_back();
				let size = stack.pop_back();
				let sha3 = self.mem.read_slice(offset, size).sha3();
				stack.push(U256::from(sha3.as_slice()));
			},
			instructions::SLOAD => {
				let key = H256::from(&stack.pop_back());
				let word = U256::from(ext.storage_at(&key).as_slice());
				stack.push(word);
			},
			instructions::SSTORE => {
				let address = H256::from(&stack.pop_back());
				let val = stack.pop_back();

				let current_val = U256::from(ext.storage_at(&address).as_slice());
				// Increase refund for clear
				if !self.is_zero(&current_val) && self.is_zero(&val) {
					ext.inc_sstore_clears();
				}
				ext.set_storage(address, H256::from(&val));
			},
			instructions::PC => {
				stack.push(U256::from(code.position - 1));
			},
			instructions::GAS => {
				stack.push(gas.as_u256());
			},
			instructions::ADDRESS => {
				stack.push(address_to_u256(params.address.clone()));
			},
			instructions::ORIGIN => {
				stack.push(address_to_u256(params.origin.clone()));
			},
			instructions::BALANCE => {
				let address = u256_to_address(&stack.pop_back());
				let balance = ext.balance(&address);
				stack.push(balance);
			},
			instructions::CALLER => {
				stack.push(address_to_u256(params.sender.clone()));
			},
			instructions::CALLVALUE => {
				stack.push(match params.value {
					ActionValue::Transfer(val) | ActionValue::Apparent(val) => val
				});
			},
			instructions::CALLDATALOAD => {
				let big_id = stack.pop_back();
				let id = big_id.low_u64() as usize;
				let max = id.wrapping_add(32);
				if let Some(data) = params.data.as_ref() {
					let bound = cmp::min(data.len(), max);
					if id < bound && big_id < U256::from(data.len()) {
						let mut v = [0u8; 32];
						v[0..bound-id].clone_from_slice(&data[id..bound]);
						stack.push(U256::from(&v[..]))
					} else {
						stack.push(U256::zero())
					}
				} else {
					stack.push(U256::zero())
				}
			},
			instructions::CALLDATASIZE => {
				stack.push(U256::from(params.data.clone().map_or(0, |l| l.len())));
			},
			instructions::CODESIZE => {
				stack.push(U256::from(code.len()));
			},
			instructions::EXTCODESIZE => {
				let address = u256_to_address(&stack.pop_back());
				let len = ext.extcode(&address).len();
				stack.push(U256::from(len));
			},
			instructions::CALLDATACOPY => {
				self.copy_data_to_memory(stack, &params.data.clone().unwrap_or_else(|| vec![]));
			},
			instructions::CODECOPY => {
				self.copy_data_to_memory(stack, &params.code.clone().unwrap_or_else(|| vec![]));
			},
			instructions::EXTCODECOPY => {
				let address = u256_to_address(&stack.pop_back());
				let code = ext.extcode(&address);
				self.copy_data_to_memory(stack, &code);
			},
			instructions::GASPRICE => {
				stack.push(params.gas_price.clone());
			},
			instructions::BLOCKHASH => {
				let block_number = stack.pop_back();
				let block_hash = ext.blockhash(&block_number);
				stack.push(U256::from(block_hash.as_slice()));
			},
			instructions::COINBASE => {
				stack.push(address_to_u256(ext.env_info().author.clone()));
			},
			instructions::TIMESTAMP => {
				stack.push(U256::from(ext.env_info().timestamp));
			},
			instructions::NUMBER => {
				stack.push(U256::from(ext.env_info().number));
			},
			instructions::DIFFICULTY => {
				stack.push(ext.env_info().difficulty.clone());
			},
			instructions::GASLIMIT => {
				stack.push(ext.env_info().gas_limit.clone());
			},
			_ => {
				try!(self.exec_stack_instruction(instruction, stack));
			}
		};
		Ok(InstructionResult::Ok)
	}

	fn copy_data_to_memory(&mut self, stack: &mut Stack<U256>, data: &[u8]) {
		let dest_offset = stack.pop_back();
		let source_offset = stack.pop_back();
		let size = stack.pop_back();
		let source_size = U256::from(data.len());

		let output_end = match source_offset > source_size || size > source_size || source_offset + size > source_size {
			true => {
				let zero_slice = if source_offset > source_size {
					self.mem.writeable_slice(dest_offset, size)
				} else {
					self.mem.writeable_slice(dest_offset + source_size - source_offset, source_offset + size - source_size)
				};
				for i in zero_slice.iter_mut() {
					*i = 0;
				}
				data.len()
			},
			false => (size.low_u64() + source_offset.low_u64()) as usize
		};

		if source_offset < source_size {
			let output_begin = source_offset.low_u64() as usize;
			self.mem.write_slice(dest_offset, &data[output_begin..output_end]);
		}
	}

	fn verify_jump(&self, jump_u: U256, valid_jump_destinations: &HashSet<usize>) -> evm::Result<usize> {
		let jump = jump_u.low_u64() as usize;

		if valid_jump_destinations.contains(&jump) && jump_u < U256::from(!0 as usize) {
			Ok(jump)
		} else {
			Err(evm::Error::BadJumpDestination {
				destination: jump
			})
		}
	}

	fn is_zero(&self, val: &U256) -> bool {
		val.is_zero()
	}

	fn bool_to_u256(&self, val: bool) -> U256 {
		if val {
			U256::one()
		} else {
			U256::zero()
		}
	}

	fn exec_stack_instruction(&self, instruction: Instruction, stack: &mut Stack<U256>) -> evm::Result<()> {
		match instruction {
			instructions::DUP1...instructions::DUP16 => {
				let position = instructions::get_dup_position(instruction);
				let val = stack.peek(position).clone();
				stack.push(val);
			},
			instructions::SWAP1...instructions::SWAP16 => {
				let position = instructions::get_swap_position(instruction);
				stack.swap_with_top(position)
			},
			instructions::POP => {
				stack.pop_back();
			},
			instructions::ADD => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a.overflowing_add(b).0);
			},
			instructions::MUL => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a.overflowing_mul(b).0);
			},
			instructions::SUB => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a.overflowing_sub(b).0);
			},
			instructions::DIV => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(if !self.is_zero(&b) {
					a.overflowing_div(b).0
				} else {
					U256::zero()
				});
			},
			instructions::MOD => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(if !self.is_zero(&b) {
					a.overflowing_rem(b).0
				} else {
					U256::zero()
				});
			},
			instructions::SDIV => {
				let (a, sign_a) = get_and_reset_sign(stack.pop_back());
				let (b, sign_b) = get_and_reset_sign(stack.pop_back());

				// -2^255
				let min = (U256::one() << 255) - U256::one();
				stack.push(if self.is_zero(&b) {
					U256::zero()
				} else if a == min && b == !U256::zero() {
					min
				} else {
					let c = a.overflowing_div(b).0;
					set_sign(c, sign_a ^ sign_b)
				});
			},
			instructions::SMOD => {
				let ua = stack.pop_back();
				let ub = stack.pop_back();
				let (a, sign_a) = get_and_reset_sign(ua);
				let b = get_and_reset_sign(ub).0;

				stack.push(if !self.is_zero(&b) {
					let c = a.overflowing_rem(b).0;
					set_sign(c, sign_a)
				} else {
					U256::zero()
				});
			},
			instructions::EXP => {
				let base = stack.pop_back();
				let expon = stack.pop_back();
				let res = base.overflowing_pow(expon).0;
				stack.push(res);
			},
			instructions::NOT => {
				let a = stack.pop_back();
				stack.push(!a);
			},
			instructions::LT => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(self.bool_to_u256(a < b));
			},
			instructions::SLT => {
				let (a, neg_a) = get_and_reset_sign(stack.pop_back());
				let (b, neg_b) = get_and_reset_sign(stack.pop_back());

				let is_positive_lt = a < b && !(neg_a | neg_b);
				let is_negative_lt = a > b && (neg_a & neg_b);
				let has_different_signs = neg_a && !neg_b;

				stack.push(self.bool_to_u256(is_positive_lt | is_negative_lt | has_different_signs));
			},
			instructions::GT => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(self.bool_to_u256(a > b));
			},
			instructions::SGT => {
				let (a, neg_a) = get_and_reset_sign(stack.pop_back());
				let (b, neg_b) = get_and_reset_sign(stack.pop_back());

				let is_positive_gt = a > b && !(neg_a | neg_b);
				let is_negative_gt = a < b && (neg_a & neg_b);
				let has_different_signs = !neg_a && neg_b;

				stack.push(self.bool_to_u256(is_positive_gt | is_negative_gt | has_different_signs));
			},
			instructions::EQ => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(self.bool_to_u256(a == b));
			},
			instructions::ISZERO => {
				let a = stack.pop_back();
				stack.push(self.bool_to_u256(self.is_zero(&a)));
			},
			instructions::AND => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a & b);
			},
			instructions::OR => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a | b);
			},
			instructions::XOR => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				stack.push(a ^ b);
			},
			instructions::BYTE => {
				let word = stack.pop_back();
				let val = stack.pop_back();
				let byte = match word < U256::from(32) {
					true => (val >> (8 * (31 - word.low_u64() as usize))) & U256::from(0xff),
					false => U256::zero()
				};
				stack.push(byte);
			},
			instructions::ADDMOD => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				let c = stack.pop_back();

				stack.push(if !self.is_zero(&c) {
					// upcast to 512
					let a5 = U512::from(a);
					let res = a5.overflowing_add(U512::from(b)).0;
					let x = res.overflowing_rem(U512::from(c)).0;
					U256::from(x)
				} else {
					U256::zero()
				});
			},
			instructions::MULMOD => {
				let a = stack.pop_back();
				let b = stack.pop_back();
				let c = stack.pop_back();

				stack.push(if !self.is_zero(&c) {
					let a5 = U512::from(a);
					let res = a5.overflowing_mul(U512::from(b)).0;
					let x = res.overflowing_rem(U512::from(c)).0;
					U256::from(x)
				} else {
					U256::zero()
				});
			},
			instructions::SIGNEXTEND => {
				let bit = stack.pop_back();
				if bit < U256::from(32) {
					let number = stack.pop_back();
					let bit_position = (bit.low_u64() * 8 + 7) as usize;

					let bit = number.bit(bit_position);
					let mask = (U256::one() << bit_position) - U256::one();
					stack.push(if bit {
						number | !mask
					} else {
						number & mask
					});
				}
			},
			_ => {
				return Err(evm::Error::BadInstruction {
					instruction: instruction
				});
			}
		}
		Ok(())
	}

	fn find_jump_destinations(&self, code: &[u8]) -> HashSet<CodePosition> {
		let mut jump_dests = HashSet::new();
		let mut position = 0;

		while position < code.len() {
			let instruction = code[position];

			if instruction == instructions::JUMPDEST {
				jump_dests.insert(position);
			} else if instructions::is_push(instruction) {
				position += instructions::get_push_bytes(instruction);
			}
			position += 1;
		}

		jump_dests
	}
}

fn get_and_reset_sign(value: U256) -> (U256, bool) {
	let U256(arr) = value;
	let sign = arr[3].leading_zeros() == 0;
	(set_sign(value, sign), sign)
}

fn set_sign(value: U256, sign: bool) -> U256 {
	if sign {
		(!U256::zero() ^ value).overflowing_add(U256::one()).0
	} else {
		value
	}
}

#[inline]
fn u256_to_address(value: &U256) -> Address {
	Address::from(H256::from(value))
}

#[inline]
fn address_to_u256(value: Address) -> U256 {
	U256::from(H256::from(value).as_slice())
}

#[test]
fn test_find_jump_destinations() {
	// given
	let interpreter = Interpreter::<U256>::default();
	let code = "7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff5b01600055".from_hex().unwrap();

	// when
	let valid_jump_destinations = interpreter.find_jump_destinations(&code);

	// then
	assert!(valid_jump_destinations.contains(&66));
}
