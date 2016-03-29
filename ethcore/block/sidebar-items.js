initSidebarItems({"fn":[["enact","Enact the block given by block header, transactions and uncles"],["enact_and_seal","Enact the block given by `block_bytes` using `engine` on the database `db` with given `parent` block header. Seal the block aferwards"],["enact_bytes","Enact the block given by `block_bytes` using `engine` on the database `db` with given `parent` block header"],["enact_verified","Enact the block given by `block_bytes` using `engine` on the database `db` with given `parent` block header"]],"struct":[["Block","A block, encoded as it is on the block chain."],["BlockRef","A set of immutable references to `ExecutedBlock` fields that are publicly accessible."],["BlockRefMut","A set of references to `ExecutedBlock` fields that are publicly accessible."],["ClosedBlock","Just like OpenBlock, except that we've applied `Engine::on_close_block`, finished up the non-seal header fields, and collected the uncles."],["ExecutedBlock","Internal type for a block's common elements."],["LockedBlock","Just like ClosedBlock except that we can't reopen it and it's faster."],["OpenBlock","Block that is ready for transactions to be added."],["SealedBlock","A block that has a valid seal."]],"trait":[["IsBlock","Trait for a object that is_a `ExecutedBlock`."]]});