// Copyright 2016 The Grin Developers
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Blocks and blockheaders

use time;
use secp::{self, Secp256k1};
use std::collections::HashSet;

use core::Committed;
use core::{Input, Output, Proof, TxKernel, Transaction, COINBASE_KERNEL, COINBASE_OUTPUT};
use consensus::REWARD;
use consensus::MINIMUM_DIFFICULTY;
use core::hash::{Hash, Hashed, ZERO_HASH};
use core::target::Difficulty;
use ser::{self, Readable, Reader, Writeable, Writer};
use global;
use keychain;


bitflags! {
    /// Options for block validation
    pub flags BlockFeatures: u8 {
        /// No flags
        const DEFAULT_BLOCK = 0b00000000,
    }
}

/// Block header, fairly standard compared to other blockchains.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockHeader {
	/// Height of this block since the genesis block (height 0)
	pub height: u64,
	/// Hash of the block previous to this in the chain.
	pub previous: Hash,
	/// Timestamp at which the block was built.
	pub timestamp: time::Tm,
	/// Merklish root of all the commitments in the UTXO set
	pub utxo_root: Hash,
	/// Merklish root of all range proofs in the UTXO set
	pub range_proof_root: Hash,
	/// Merklish root of all transaction kernels in the UTXO set
	pub kernel_root: Hash,
	/// Features specific to this block, allowing possible future extensions
	pub features: BlockFeatures,
	/// Nonce increment used to mine this block.
	pub nonce: u64,
	/// Proof of work data.
	pub pow: Proof,
	/// Difficulty used to mine the block.
	pub difficulty: Difficulty,
	/// Total accumulated difficulty since genesis block
	pub total_difficulty: Difficulty,
}

impl Default for BlockHeader {
	fn default() -> BlockHeader {
		let proof_size = global::proofsize();
		BlockHeader {
			height: 0,
			previous: ZERO_HASH,
			timestamp: time::at_utc(time::Timespec { sec: 0, nsec: 0 }),
			difficulty: Difficulty::from_num(MINIMUM_DIFFICULTY),
			total_difficulty: Difficulty::from_num(MINIMUM_DIFFICULTY),
			utxo_root: ZERO_HASH,
			range_proof_root: ZERO_HASH,
			kernel_root: ZERO_HASH,
			features: DEFAULT_BLOCK,
			nonce: 0,
			pow: Proof::zero(proof_size),
		}
	}
}

/// Serialization of a block header
impl Writeable for BlockHeader {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		ser_multiwrite!(
			writer,
			[write_u64, self.height],
			[write_fixed_bytes, &self.previous],
			[write_i64, self.timestamp.to_timespec().sec],
			[write_fixed_bytes, &self.utxo_root],
			[write_fixed_bytes, &self.range_proof_root],
			[write_fixed_bytes, &self.kernel_root],
			[write_u8, self.features.bits()]
		);

		try!(writer.write_u64(self.nonce));
		try!(self.difficulty.write(writer));
		try!(self.total_difficulty.write(writer));

		if writer.serialization_mode() != ser::SerializationMode::Hash {
			try!(self.pow.write(writer));
		}
		Ok(())
	}
}

/// Deserialization of a block header
impl Readable for BlockHeader {
	fn read(reader: &mut Reader) -> Result<BlockHeader, ser::Error> {
		let height = try!(reader.read_u64());
		let previous = try!(Hash::read(reader));
		let timestamp = reader.read_i64()?;
		let utxo_root = try!(Hash::read(reader));
		let rproof_root = try!(Hash::read(reader));
		let kernel_root = try!(Hash::read(reader));
		let (features, nonce) = ser_multiread!(reader, read_u8, read_u64);
		let difficulty = try!(Difficulty::read(reader));
		let total_difficulty = try!(Difficulty::read(reader));
		let pow = try!(Proof::read(reader));

		Ok(BlockHeader {
			height: height,
			previous: previous,
			timestamp: time::at_utc(time::Timespec {
				sec: timestamp,
				nsec: 0,
			}),
			utxo_root: utxo_root,
			range_proof_root: rproof_root,
			kernel_root: kernel_root,
			features: BlockFeatures::from_bits(features).ok_or(
				ser::Error::CorruptedData,
			)?,
			pow: pow,
			nonce: nonce,
			difficulty: difficulty,
			total_difficulty: total_difficulty,
		})
	}
}

/// A block as expressed in the MimbleWimble protocol. The reward is
/// non-explicit, assumed to be deducible from block height (similar to
/// bitcoin's schedule) and expressed as a global transaction fee (added v.H),
/// additive to the total of fees ever collected.
#[derive(Debug, Clone)]
pub struct Block {
	/// The header with metadata and commitments to the rest of the data
	pub header: BlockHeader,
	/// List of transaction inputs
	pub inputs: Vec<Input>,
	/// List of transaction outputs
	pub outputs: Vec<Output>,
	/// List of transaction kernels and associated proofs
	pub kernels: Vec<TxKernel>,
}

/// Implementation of Writeable for a block, defines how to write the block to a
/// binary writer. Differentiates between writing the block for the purpose of
/// full serialization and the one of just extracting a hash.
impl Writeable for Block {
	fn write<W: Writer>(&self, writer: &mut W) -> Result<(), ser::Error> {
		try!(self.header.write(writer));

		if writer.serialization_mode() != ser::SerializationMode::Hash {
			ser_multiwrite!(
				writer,
				[write_u64, self.inputs.len() as u64],
				[write_u64, self.outputs.len() as u64],
				[write_u64, self.kernels.len() as u64]
			);

			for inp in &self.inputs {
				try!(inp.write(writer));
			}
			for out in &self.outputs {
				try!(out.write(writer));
			}
			for proof in &self.kernels {
				try!(proof.write(writer));
			}
		}
		Ok(())
	}
}

/// Implementation of Readable for a block, defines how to read a full block
/// from a binary stream.
impl Readable for Block {
	fn read(reader: &mut Reader) -> Result<Block, ser::Error> {
		let header = try!(BlockHeader::read(reader));

		let (input_len, output_len, proof_len) =
			ser_multiread!(reader, read_u64, read_u64, read_u64);

		let inputs = try!((0..input_len).map(|_| Input::read(reader)).collect());
		let outputs = try!((0..output_len).map(|_| Output::read(reader)).collect());
		let kernels = try!((0..proof_len).map(|_| TxKernel::read(reader)).collect());

		Ok(Block {
			header: header,
			inputs: inputs,
			outputs: outputs,
			kernels: kernels,
			..Default::default()
		})
	}
}

/// Provides all information from a block that allows the calculation of total
/// Pedersen commitment.
impl Committed for Block {
	fn inputs_committed(&self) -> &Vec<Input> {
		&self.inputs
	}
	fn outputs_committed(&self) -> &Vec<Output> {
		&self.outputs
	}
	fn overage(&self) -> i64 {
		(self.total_fees() as i64) - (REWARD as i64)
	}
}

/// Default properties for a block, everything zeroed out and empty vectors.
impl Default for Block {
	fn default() -> Block {
		Block {
			header: Default::default(),
			inputs: vec![],
			outputs: vec![],
			kernels: vec![],
		}
	}
}

impl Block {
	/// Builds a new block from the header of the previous block, a vector of
	/// transactions and the private key that will receive the reward. Checks
	/// that all transactions are valid and calculates the Merkle tree.
	pub fn new(
		prev: &BlockHeader,
		txs: Vec<&Transaction>,
		keychain: &keychain::Keychain,
		pubkey: keychain::Identifier,
	) -> Result<Block, keychain::Error> {

		let (reward_out, reward_proof) = Block::reward_output(keychain, pubkey)?;
		let block = Block::with_reward(prev, txs, reward_out, reward_proof)?;
		Ok(block)
	}

	/// Builds a new block ready to mine from the header of the previous block,
	/// a vector of transactions and the reward information. Checks
	/// that all transactions are valid and calculates the Merkle tree.
	pub fn with_reward(
		prev: &BlockHeader,
		txs: Vec<&Transaction>,
		reward_out: Output,
		reward_kern: TxKernel,
	) -> Result<Block, secp::Error> {
		// note: the following reads easily but may not be the most efficient due to
		// repeated iterations, revisit if a problem
		let secp = Secp256k1::with_caps(secp::ContextFlag::Commit);

		// validate each transaction and gather their kernels
		let mut kernels = try_map_vec!(txs, |tx| tx.verify_sig(&secp));
		kernels.push(reward_kern);

		// build vectors with all inputs and all outputs, ordering them by hash
		// needs to be a fold so we don't end up with a vector of vectors and we
		// want to fully own the refs (not just a pointer like flat_map).
		let mut inputs = txs.iter().fold(vec![], |mut acc, ref tx| {
			let mut inputs = tx.inputs.clone();
			acc.append(&mut inputs);
			acc
		});
		let mut outputs = txs.iter().fold(vec![], |mut acc, ref tx| {
			let mut outputs = tx.outputs.clone();
			acc.append(&mut outputs);
			acc
		});
		outputs.push(reward_out);

		inputs.sort_by_key(|inp| inp.hash());
		outputs.sort_by_key(|out| out.hash());

		// calculate the overall Merkle tree and fees

		Ok(
			Block {
				header: BlockHeader {
					height: prev.height + 1,
					timestamp: time::Tm {
						tm_nsec: 0,
						..time::now_utc()
					},
					previous: prev.hash(),
					total_difficulty: prev.pow.clone().to_difficulty() +
						prev.total_difficulty.clone(),
					..Default::default()
				},
				inputs: inputs,
				outputs: outputs,
				kernels: kernels,
			}.compact(),
		)
	}


	/// Blockhash, computed using only the header
	pub fn hash(&self) -> Hash {
		self.header.hash()
	}

	/// Sum of all fees (inputs less outputs) in the block
	pub fn total_fees(&self) -> u64 {
		self.kernels.iter().map(|p| p.fee).sum()
	}

	/// Matches any output with a potential spending input, eliminating them
	/// from the block. Provides a simple way to compact the block. The
	/// elimination is stable with respect to inputs and outputs order.
	///
	/// NOTE: exclude coinbase from compaction process
	/// if a block contains a new coinbase output and
	/// is a transaction spending a previous coinbase
	/// we do not want to compact these away
	///
	pub fn compact(&self) -> Block {
		let in_set = self.inputs
			.iter()
			.map(|inp| inp.commitment())
			.collect::<HashSet<_>>();

		let out_set = self.outputs
			.iter()
			.filter(|out| !out.features.contains(COINBASE_OUTPUT))
			.map(|out| out.commitment())
			.collect::<HashSet<_>>();

		let commitments_to_compact = in_set.intersection(&out_set).collect::<HashSet<_>>();

		let new_inputs = self.inputs
			.iter()
			.filter(|inp| !commitments_to_compact.contains(&inp.commitment()))
			.map(|&inp| inp)
			.collect::<Vec<_>>();

		let new_outputs = self.outputs
			.iter()
			.filter(|out| !commitments_to_compact.contains(&out.commitment()))
			.map(|&out| out)
			.collect::<Vec<_>>();

		Block {
			header: BlockHeader {
				pow: self.header.pow.clone(),
				difficulty: self.header.difficulty.clone(),
				total_difficulty: self.header.total_difficulty.clone(),
				..self.header
			},
			inputs: new_inputs,
			outputs: new_outputs,
			kernels: self.kernels.clone(),
		}
	}

	/// Merges the 2 blocks, essentially appending the inputs, outputs and
	/// kernels.
	/// Also performs a compaction on the result.
	pub fn merge(&self, other: Block) -> Block {
		let mut all_inputs = self.inputs.clone();
		all_inputs.append(&mut other.inputs.clone());

		let mut all_outputs = self.outputs.clone();
		all_outputs.append(&mut other.outputs.clone());

		let mut all_kernels = self.kernels.clone();
		all_kernels.append(&mut other.kernels.clone());

		all_inputs.sort_by_key(|inp| inp.hash());
		all_outputs.sort_by_key(|out| out.hash());

		Block {
			// compact will fix the merkle tree
			header: BlockHeader {
				pow: self.header.pow.clone(),
				difficulty: self.header.difficulty.clone(),
				total_difficulty: self.header.total_difficulty.clone(),
				..self.header
			},
			inputs: all_inputs,
			outputs: all_outputs,
			kernels: all_kernels,
		}.compact()
	}

	/// Validates all the elements in a block that can be checked without
	/// additional
	/// data. Includes commitment sums and kernels, Merkle trees, reward, etc.
	pub fn validate(&self, secp: &Secp256k1) -> Result<(), secp::Error> {
		self.verify_coinbase(secp)?;
		self.verify_kernels(secp)?;
		Ok(())
	}

	/// Validate the sum of input/output commitments match the sum in kernels
	/// and
	/// that all kernel signatures are valid.
	pub fn verify_kernels(&self, secp: &Secp256k1) -> Result<(), secp::Error> {
		// sum all inputs and outs commitments
		let io_sum = self.sum_commitments(secp)?;

		// sum all kernels commitments
		let proof_commits = map_vec!(self.kernels, |proof| proof.excess);
		let proof_sum = secp.commit_sum(proof_commits, vec![])?;

		// both should be the same
		if proof_sum != io_sum {
			// TODO more specific error
			return Err(secp::Error::IncorrectCommitSum);
		}

		// verify all signatures with the commitment as pk
		for proof in &self.kernels {
			proof.verify(secp)?;
		}
		Ok(())
	}

	// Validate the coinbase outputs generated by miners. Entails 2 main checks:
	//
	// * That the sum of all coinbase-marked outputs equal the supply.
	// * That the sum of blinding factors for all coinbase-marked outputs match
	//   the coinbase-marked kernels.
	fn verify_coinbase(&self, secp: &Secp256k1) -> Result<(), secp::Error> {
		let cb_outs = self.outputs
			.iter()
			.filter(|out| out.features.contains(COINBASE_OUTPUT))
			.map(|o| o.clone())
			.collect::<Vec<_>>();
		let cb_kerns = self.kernels
			.iter()
			.filter(|k| k.features.contains(COINBASE_KERNEL))
			.map(|k| k.clone())
			.collect::<Vec<_>>();

		// verifying the kernels on a block composed of just the coinbase outputs
		// and kernels checks all we need
		Block {
			header: BlockHeader::default(),
			inputs: vec![],
			outputs: cb_outs,
			kernels: cb_kerns,
		}.verify_kernels(secp)
	}

	/// Builds the blinded output and related signature proof for the block
	/// reward.
	pub fn reward_output(
		keychain: &keychain::Keychain,
		pubkey: keychain::Identifier,
	) -> Result<(Output, TxKernel), keychain::Error> {
		let secp = keychain.secp();

		let msg = secp::Message::from_slice(&[0; secp::constants::MESSAGE_SIZE])?;
		let sig = keychain.sign(&msg, &pubkey)?;
		let commit = keychain.commit(REWARD, &pubkey)?;
		// let switch_commit = keychain.switch_commit(pubkey)?;

		let rproof = keychain.range_proof(REWARD, &pubkey, commit)?;

		let output = Output {
			features: COINBASE_OUTPUT,
			commit: commit,
			proof: rproof,
		};

		let over_commit = try!(secp.commit_value(REWARD as u64));
		let out_commit = output.commitment();
		let excess = try!(secp.commit_sum(vec![out_commit], vec![over_commit]));

		let proof = TxKernel {
			features: COINBASE_KERNEL,
			excess: excess,
			excess_sig: sig.serialize_der(&secp),
			fee: 0,
		};
		Ok((output, proof))
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use core::Transaction;
	use core::build::{self, input, output, with_fee};
	use core::test::tx2i1o;
	use keychain::{Identifier, Keychain};

	use secp;

	// utility to create a block without worrying about the key or previous
	// header
	fn new_block(txs: Vec<&Transaction>, keychain: &Keychain) -> Block {
		let pubkey = keychain.derive_pubkey(1).unwrap();
		Block::new(&BlockHeader::default(), txs, keychain, pubkey).unwrap()
	}

	// utility producing a transaction that spends an output with the provided
	// value and blinding key
	fn txspend1i1o(v: u64, keychain: &Keychain, pk1: Identifier, pk2: Identifier) -> Transaction {
		build::transaction(vec![input(v, pk1), output(3, pk2), with_fee(1)], &keychain)
			.map(|(tx, _)| tx)
			.unwrap()
	}

	#[test]
	// builds a block with a tx spending another and check if merging occurred
	fn compactable_block() {
		let keychain = Keychain::from_random_seed().unwrap();
		let pk1 = keychain.derive_pubkey(1).unwrap();
		let pk2 = keychain.derive_pubkey(2).unwrap();
		let pk3 = keychain.derive_pubkey(3).unwrap();

		let mut btx1 = tx2i1o();
		let (mut btx2, _) = build::transaction(
			vec![input(5, pk1), output(4, pk2.clone()), with_fee(1)],
			&keychain,
		).unwrap();

		// spending tx2 - reuse pk2

		let mut btx3 = txspend1i1o(4, &keychain, pk2.clone(), pk3);
		let b = new_block(vec![&mut btx1, &mut btx2, &mut btx3], &keychain);

		// block should have been automatically compacted (including reward
		// output) and should still be valid
		b.validate(&keychain.secp()).unwrap();
		assert_eq!(b.inputs.len(), 3);
		assert_eq!(b.outputs.len(), 3);
	}

	#[test]
	// builds 2 different blocks with a tx spending another and check if merging
	// occurs
	fn mergeable_blocks() {
		let keychain = Keychain::from_random_seed().unwrap();
		let pk1 = keychain.derive_pubkey(1).unwrap();
		let pk2 = keychain.derive_pubkey(2).unwrap();
		let pk3 = keychain.derive_pubkey(3).unwrap();

		let mut btx1 = tx2i1o();

		let (mut btx2, _) = build::transaction(
			vec![input(5, pk1), output(4, pk2.clone()), with_fee(1)],
			&keychain,
		).unwrap();

		// spending tx2 - reuse pk2
		let mut btx3 = txspend1i1o(4, &keychain, pk2.clone(), pk3);

		let b1 = new_block(vec![&mut btx1, &mut btx2], &keychain);
		b1.validate(&keychain.secp()).unwrap();

		let b2 = new_block(vec![&mut btx3], &keychain);
		b2.validate(&keychain.secp()).unwrap();

		// block should have been automatically compacted and should still be valid
		let b3 = b1.merge(b2);
		assert_eq!(b3.inputs.len(), 3);
		assert_eq!(b3.outputs.len(), 4);
	}

	#[test]
	fn empty_block_with_coinbase_is_valid() {
		let keychain = Keychain::from_random_seed().unwrap();
		let b = new_block(vec![], &keychain);

		assert_eq!(b.inputs.len(), 0);
		assert_eq!(b.outputs.len(), 1);
		assert_eq!(b.kernels.len(), 1);

		let coinbase_outputs = b.outputs
			.iter()
			.filter(|out| out.features.contains(COINBASE_OUTPUT))
			.map(|o| o.clone())
			.collect::<Vec<_>>();
		assert_eq!(coinbase_outputs.len(), 1);

		let coinbase_kernels = b.kernels
			.iter()
			.filter(|out| out.features.contains(COINBASE_KERNEL))
			.map(|o| o.clone())
			.collect::<Vec<_>>();
		assert_eq!(coinbase_kernels.len(), 1);

		// the block should be valid here (single coinbase output with corresponding
		// txn kernel)
		assert_eq!(b.validate(&keychain.secp()), Ok(()));
	}

	#[test]
	// test that flipping the COINBASE_OUTPUT flag on the output features
	// invalidates the block and specifically it causes verify_coinbase to fail
	// additionally verifying the merkle_inputs_outputs also fails
	fn remove_coinbase_output_flag() {
		let keychain = Keychain::from_random_seed().unwrap();
		let mut b = new_block(vec![], &keychain);

		assert!(b.outputs[0].features.contains(COINBASE_OUTPUT));
		b.outputs[0].features.remove(COINBASE_OUTPUT);

		assert_eq!(
			b.verify_coinbase(&keychain.secp()),
			Err(secp::Error::IncorrectCommitSum)
		);
		assert_eq!(b.verify_kernels(&keychain.secp()), Ok(()));

		assert_eq!(
			b.validate(&keychain.secp()),
			Err(secp::Error::IncorrectCommitSum)
		);
	}

	#[test]
	// test that flipping the COINBASE_KERNEL flag on the kernel features
	// invalidates the block and specifically it causes verify_coinbase to fail
	fn remove_coinbase_kernel_flag() {
		let keychain = Keychain::from_random_seed().unwrap();
		let mut b = new_block(vec![], &keychain);

		assert!(b.kernels[0].features.contains(COINBASE_KERNEL));
		b.kernels[0].features.remove(COINBASE_KERNEL);

		assert_eq!(
			b.verify_coinbase(&keychain.secp()),
			Err(secp::Error::IncorrectCommitSum)
		);
		assert_eq!(b.verify_kernels(&keychain.secp()), Ok(()));

		assert_eq!(
			b.validate(&keychain.secp()),
			Err(secp::Error::IncorrectCommitSum)
		);
	}

	#[test]
	fn serialize_deserialize_block() {
		let keychain = Keychain::from_random_seed().unwrap();
		let b = new_block(vec![], &keychain);

		let mut vec = Vec::new();
		ser::serialize(&mut vec, &b).expect("serialization failed");
		let b2: Block = ser::deserialize(&mut &vec[..]).unwrap();

		assert_eq!(b.inputs, b2.inputs);
		assert_eq!(b.outputs, b2.outputs);
		assert_eq!(b.kernels, b2.kernels);
		assert_eq!(b.header, b2.header);
	}
}
