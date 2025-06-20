use alloc::{collections::BTreeMap, vec::Vec};

use vm_core::{
    crypto::merkle::{MerkleStore, NodeIndex, StoreNode},
    mast::MastNodeExt,
};

use super::{
    AdviceInputs, AdviceProvider, AdviceSource, ExecutionError, Felt, MerklePath, RpoDigest, Word,
};
use crate::{
    ErrorContext, ProcessState,
    utils::collections::{KvMap, RecordingMap},
};

// TYPE ALIASES
// ================================================================================================

type SimpleMerkleMap = BTreeMap<RpoDigest, StoreNode>;
type RecordingMerkleMap = RecordingMap<RpoDigest, StoreNode>;

type SimpleAdviceMap = BTreeMap<RpoDigest, Vec<Felt>>;
type RecordingAdviceMap = RecordingMap<RpoDigest, Vec<Felt>>;

// BASE ADVICE PROVIDER
// ================================================================================================

/// An in-memory [AdviceProvider] implementation which serves as the base for advice providers
/// bundles with Miden VM.
#[derive(Debug, Clone, Default)]
pub struct BaseAdviceProvider<M, S>
where
    M: KvMap<RpoDigest, Vec<Felt>>,
    S: KvMap<RpoDigest, StoreNode>,
{
    stack: Vec<Felt>,
    map: M,
    store: MerkleStore<S>,
}

impl<M, S> From<AdviceInputs> for BaseAdviceProvider<M, S>
where
    M: KvMap<RpoDigest, Vec<Felt>>,
    S: KvMap<RpoDigest, StoreNode>,
{
    fn from(inputs: AdviceInputs) -> Self {
        let (mut stack, map, store) = inputs.into_parts();
        stack.reverse();
        Self {
            stack,
            map: map.into_iter().collect(),
            store: store.inner_nodes().collect(),
        }
    }
}

impl<M, S> AdviceProvider for BaseAdviceProvider<M, S>
where
    M: KvMap<RpoDigest, Vec<Felt>>,
    S: KvMap<RpoDigest, StoreNode>,
{
    // ADVICE STACK
    // --------------------------------------------------------------------------------------------

    fn pop_stack(
        &mut self,
        process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Felt, ExecutionError> {
        self.stack
            .pop()
            .ok_or(ExecutionError::advice_stack_read_failed(process.clk(), err_ctx))
    }

    fn pop_stack_word(
        &mut self,
        process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        if self.stack.len() < 4 {
            return Err(ExecutionError::advice_stack_read_failed(process.clk(), err_ctx));
        }

        let idx = self.stack.len() - 4;
        let result =
            [self.stack[idx + 3], self.stack[idx + 2], self.stack[idx + 1], self.stack[idx]];

        self.stack.truncate(idx);

        Ok(result)
    }

    fn pop_stack_dword(
        &mut self,
        process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<[Word; 2], ExecutionError> {
        let word0 = self.pop_stack_word(process, err_ctx)?;
        let word1 = self.pop_stack_word(process, err_ctx)?;

        Ok([word0, word1])
    }

    fn push_stack(
        &mut self,
        source: AdviceSource,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<(), ExecutionError> {
        match source {
            AdviceSource::Value(value) => {
                self.stack.push(value);
            },
            AdviceSource::Word(word) => {
                self.stack.extend(word.iter().rev());
            },
            AdviceSource::Map { key, include_len } => {
                let values = self
                    .map
                    .get(&key.into())
                    .ok_or(ExecutionError::advice_map_key_not_found(key, err_ctx))?;

                self.stack.extend(values.iter().rev());
                if include_len {
                    self.stack
                        .push(Felt::try_from(values.len() as u64).expect("value length too big"));
                }
            },
        }

        Ok(())
    }

    fn peek_stack(&self, length: usize) -> &[Felt] {
        if length == 0 {
            &self.stack
        } else {
            &self.stack[0..length]
        }
    }

    // ADVICE MAP
    // --------------------------------------------------------------------------------------------

    fn get_mapped_values(&self, key: &RpoDigest) -> Option<&[Felt]> {
        self.map.get(key).map(|v| v.as_slice())
    }

    fn insert_into_map(&mut self, key: Word, values: Vec<Felt>) {
        self.map.insert(key.into(), values);
    }

    // MERKLE STORE
    // --------------------------------------------------------------------------------------------

    fn get_tree_node(
        &self,
        root: Word,
        depth: &Felt,
        index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        let index = NodeIndex::from_elements(depth, index)
            .map_err(|_| ExecutionError::invalid_merkle_tree_node_index(*depth, *index, err_ctx))?;
        self.store
            .get_node(root.into(), index)
            .map(|v| v.into())
            .map_err(|err| ExecutionError::merkle_store_lookup_failed(err, err_ctx))
    }

    fn get_merkle_path(
        &self,
        root: Word,
        depth: &Felt,
        index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<MerklePath, ExecutionError> {
        let index = NodeIndex::from_elements(depth, index)
            .map_err(|_| ExecutionError::invalid_merkle_tree_node_index(*depth, *index, err_ctx))?;
        self.store
            .get_path(root.into(), index)
            .map(|value| value.path)
            .map_err(|err| ExecutionError::merkle_store_lookup_failed(err, err_ctx))
    }

    fn get_leaf_depth(
        &self,
        root: Word,
        tree_depth: &Felt,
        index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<u8, ExecutionError> {
        let tree_depth = u8::try_from(tree_depth.as_int())
            .map_err(|_| ExecutionError::invalid_merkle_tree_depth(*tree_depth, err_ctx))?;
        self.store
            .get_leaf_depth(root.into(), tree_depth, index.as_int())
            .map_err(|err| ExecutionError::merkle_store_lookup_failed(err, err_ctx))
    }

    fn update_merkle_node(
        &mut self,
        root: Word,
        depth: &Felt,
        index: &Felt,
        value: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<(MerklePath, Word), ExecutionError> {
        let node_index = NodeIndex::from_elements(depth, index)
            .map_err(|_| ExecutionError::invalid_merkle_tree_node_index(*depth, *index, err_ctx))?;
        self.store
            .set_node(root.into(), node_index, value.into())
            .map(|root| (root.path, root.root.into()))
            .map_err(|err| ExecutionError::merkle_store_update_failed(err, err_ctx))
    }

    fn merge_roots(
        &mut self,
        lhs: Word,
        rhs: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.store
            .merge_roots(lhs.into(), rhs.into())
            .map(|v| v.into())
            .map_err(|err| ExecutionError::merkle_store_merge_failed(err, err_ctx))
    }
}

// MEMORY ADVICE PROVIDER
// ================================================================================================

/// An in-memory `[AdviceProvider]` implementation which uses [BTreeMap]s as its backing storage.
#[derive(Debug, Clone, Default)]
pub struct MemAdviceProvider {
    provider: BaseAdviceProvider<SimpleAdviceMap, SimpleMerkleMap>,
}

impl From<AdviceInputs> for MemAdviceProvider {
    fn from(inputs: AdviceInputs) -> Self {
        let provider = inputs.into();
        Self { provider }
    }
}

/// Accessors to internal data structures of the provider used for testing purposes.
#[cfg(any(test, feature = "testing"))]
impl MemAdviceProvider {
    /// Returns the current state of the advice stack.
    pub fn stack(&self) -> &[Felt] {
        &self.provider.stack
    }

    /// Returns the current state of the advice map.
    pub fn map(&self) -> &SimpleAdviceMap {
        &self.provider.map
    }

    // Returns the current state of the Merkle store.
    pub fn store(&self) -> &MerkleStore<SimpleMerkleMap> {
        &self.provider.store
    }

    /// Returns true if the Merkle root exists for the advice provider Merkle store.
    pub fn has_merkle_root(&self, root: crate::crypto::RpoDigest) -> bool {
        self.provider.store.get_node(root, NodeIndex::root()).is_ok()
    }
}

/// Pass-through implementations of [AdviceProvider] methods.
///
/// TODO: potentially do this via a macro.
#[rustfmt::skip]
impl AdviceProvider for MemAdviceProvider {
    fn pop_stack(&mut self, process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    )-> Result<Felt, ExecutionError> {
        self.provider.pop_stack(process, err_ctx)
    }

    fn pop_stack_word(&mut self, process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.pop_stack_word(process, err_ctx)
    }

    fn pop_stack_dword(&mut self, process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<[Word; 2], ExecutionError> {
        self.provider.pop_stack_dword(process, err_ctx)
    }

    fn push_stack(&mut self, source: AdviceSource, err_ctx: &ErrorContext<impl MastNodeExt>) -> Result<(), ExecutionError> {
        self.provider.push_stack(source, err_ctx)
    }

    fn peek_stack(&self, length: usize,
    )-> &[Felt] {
        self.provider.peek_stack(length)
    }

    fn insert_into_map(&mut self, key: Word, values: Vec<Felt>)  {
        self.provider.insert_into_map(key, values)
    }

    fn get_mapped_values(&self, key: &RpoDigest) -> Option<&[Felt]> {
        self.provider.get_mapped_values(key)
    }

    fn get_tree_node(&self, root: Word, depth: &Felt, index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.get_tree_node(root, depth, index, err_ctx)
    }

    fn get_merkle_path(&self, root: Word, depth: &Felt, index: &Felt, 
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<MerklePath, ExecutionError> {
        self.provider.get_merkle_path(root, depth, index, err_ctx)
    }

    fn get_leaf_depth(&self, root: Word, tree_depth: &Felt, index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<u8, ExecutionError> {
        self.provider.get_leaf_depth(root, tree_depth, index, err_ctx)
    }

    fn update_merkle_node(&mut self, root: Word, depth: &Felt, index: &Felt, value: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<(MerklePath, Word), ExecutionError> {
        self.provider.update_merkle_node(root, depth, index, value, err_ctx)
    }

    fn merge_roots(&mut self, lhs: Word, rhs: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.merge_roots(lhs, rhs, err_ctx)
    }
}

impl MemAdviceProvider {
    // FINALIZATION
    // --------------------------------------------------------------------------------------------
    /// Consumes the [MemAdviceProvider] and returns a `(Vec<Felt>, SimpleAdviceMap, MerkleStore)`,
    /// containing the stack, map, store respectively, of the advice provider.
    pub fn into_parts(self) -> (Vec<Felt>, SimpleAdviceMap, MerkleStore) {
        let BaseAdviceProvider { stack, map, store } = self.provider;
        (stack, map, store)
    }
}

// RECORDING ADVICE PROVIDER
// ================================================================================================

/// An in-memory `[AdviceProvider]` implementation with support for data access recording.
///
/// The recorder can be converted into a proof which can be used to provide the non-deterministic
/// inputs for program execution.
#[derive(Debug, Clone, Default)]
pub struct RecAdviceProvider {
    provider: BaseAdviceProvider<RecordingAdviceMap, RecordingMerkleMap>,
    init_stack: Vec<Felt>,
}

impl From<AdviceInputs> for RecAdviceProvider {
    fn from(inputs: AdviceInputs) -> Self {
        let init_stack = inputs.stack().to_vec();
        let provider = inputs.into();
        Self { provider, init_stack }
    }
}

/// Accessors to internal data structures of the provider used for testing purposes.
#[cfg(any(test, feature = "testing"))]
impl RecAdviceProvider {
    /// Returns the current state of the advice stack.
    pub fn stack(&self) -> &[Felt] {
        &self.provider.stack
    }

    /// Returns the current state of the advice map.
    pub fn map(&self) -> &RecordingAdviceMap {
        &self.provider.map
    }

    // Returns the current state of the Merkle store.
    pub fn store(&self) -> &MerkleStore<RecordingMerkleMap> {
        &self.provider.store
    }

    /// Returns true if the Merkle root exists for the advice provider Merkle store.
    pub fn has_merkle_root(&self, root: crate::crypto::RpoDigest) -> bool {
        self.provider.store.get_node(root, NodeIndex::root()).is_ok()
    }
}

/// Pass-through implementations of [AdviceProvider] methods.
///
/// TODO: potentially do this via a macro.
#[rustfmt::skip]
impl AdviceProvider for RecAdviceProvider {
    fn pop_stack(&mut self, process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Felt, ExecutionError> {
        self.provider.pop_stack(process,err_ctx)
    }

    fn pop_stack_word(&mut self, process: ProcessState, 
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.pop_stack_word(process,err_ctx)
    }

    fn pop_stack_dword(&mut self, process: ProcessState,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<[Word; 2], ExecutionError> {
        self.provider.pop_stack_dword(process, err_ctx)
    }

    fn push_stack(&mut self, source: AdviceSource, err_ctx: &ErrorContext<impl MastNodeExt>) -> Result<(), ExecutionError> {
        self.provider.push_stack(source, err_ctx)
    }

    fn peek_stack(&self, length: usize) -> &[Felt] {
        self.provider.peek_stack(length)
    }

    fn insert_into_map(&mut self, key: Word, values: Vec<Felt>)  {
        self.provider.insert_into_map(key, values)
    }

    fn get_mapped_values(&self, key: &RpoDigest) -> Option<&[Felt]> {
        self.provider.get_mapped_values(key)
    }

    fn get_tree_node(&self, root: Word, depth: &Felt, index: &Felt,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.get_tree_node(root, depth, index, err_ctx)
    }

    fn get_merkle_path(&self, root: Word, depth: &Felt, index: &Felt, 
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<MerklePath, ExecutionError> {
        self.provider.get_merkle_path(root, depth, index, err_ctx)
    }

    fn get_leaf_depth(&self, root: Word, tree_depth: &Felt, index: &Felt, 
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<u8, ExecutionError> {
        self.provider.get_leaf_depth(root, tree_depth, index, err_ctx)
    }

    fn update_merkle_node(&mut self, root: Word, depth: &Felt, index: &Felt, value: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<(MerklePath, Word), ExecutionError> {
        self.provider.update_merkle_node(root, depth, index, value, err_ctx)
    }

    fn merge_roots(&mut self, lhs: Word, rhs: Word,
        err_ctx: &ErrorContext<'_, impl MastNodeExt>,
    ) -> Result<Word, ExecutionError> {
        self.provider.merge_roots(lhs, rhs, err_ctx)
    }
}

impl RecAdviceProvider {
    // FINALIZATION
    // --------------------------------------------------------------------------------------------

    /// Consumes the advice provider and returns an `(AdviceInputs, Vec<Felt>, SimpleAdviceMap,
    /// MerkleStore)` tuple.
    ///
    /// The [AdviceInputs] can be used to re-execute the program. The returned [AdviceInputs]
    /// instance will contain only the non-deterministic inputs which were requested during program
    /// execution.
    ///
    /// The `Vec<Felt>`, `SimpleAdviceMap`, and `MerkleStore` represent the stack, map, and Merkle
    /// store of the advice provider at the time of finalization.
    pub fn finalize(self) -> (AdviceInputs, Vec<Felt>, SimpleAdviceMap, MerkleStore) {
        let Self { provider, init_stack } = self;
        let BaseAdviceProvider { stack, map, store } = provider;

        let (map, map_proof) = map.finalize();
        let (store, store_proof) = store.into_inner().finalize();

        let proof = AdviceInputs::default()
            .with_stack(init_stack)
            .with_map(map_proof)
            .with_merkle_store(store_proof.into());

        (proof, stack, map, store.into())
    }
}
