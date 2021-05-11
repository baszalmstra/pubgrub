// SPDX-License-Identifier: MPL-2.0

//! Core model and functions
//! to write a functional PubGrub algorithm.

use std::collections::HashSet as Set;

use crate::error::PubGrubError;
use crate::internal::arena::Arena;
use crate::internal::assignment::Assignment::{Decision, Derivation};
use crate::internal::incompatibility::IncompId;
use crate::internal::incompatibility::{Incompatibility, Relation};
use crate::internal::partial_solution::{DecisionLevel, PartialSolution};
use crate::package::Package;
use crate::range::RangeSet;
use crate::report::DerivationTree;
use crate::solver::DependencyConstraints;
use crate::type_aliases::Map;

/// Current state of the PubGrub algorithm.
#[derive(Clone)]
pub struct State<P: Package, R: RangeSet> {
    root_package: P,
    root_version: R::VERSION,

    incompatibilities: Map<P, Vec<IncompId<P, R>>>,
    used_incompatibilities: rustc_hash::FxHashSet<IncompId<P, R>>,

    /// Partial solution.
    /// TODO: remove pub.
    pub partial_solution: PartialSolution<P, R>,

    /// The store is the reference storage for all incompatibilities.
    pub incompatibility_store: Arena<Incompatibility<P, R>>,

    /// This is a stack of work to be done in `unit_propagation`.
    /// It can definitely be a local variable to that method, but
    /// this way we can reuse the same allocation for better performance.
    unit_propagation_buffer: Vec<P>,
}

impl<P: Package, R: RangeSet> State<P, R> {
    /// Initialization of PubGrub state.
    pub fn init(root_package: P, root_version: R::VERSION) -> Self {
        let mut incompatibility_store = Arena::new();
        let not_root_id = incompatibility_store.alloc(Incompatibility::not_root(
            root_package.clone(),
            root_version.clone(),
        ));
        let mut incompatibilities = Map::default();
        incompatibilities.insert(root_package.clone(), vec![not_root_id]);
        Self {
            root_package,
            root_version,
            incompatibilities,
            used_incompatibilities: rustc_hash::FxHashSet::default(),
            partial_solution: PartialSolution::empty(),
            incompatibility_store,
            unit_propagation_buffer: vec![],
        }
    }

    /// Add an incompatibility to the state.
    pub fn add_incompatibility(&mut self, incompat: Incompatibility<P, R>) {
        let id = self.incompatibility_store.alloc(incompat);
        self.merge_incompatibility(id);
    }

    /// Add an incompatibility to the state.
    pub fn add_incompatibility_from_dependencies(
        &mut self,
        package: P,
        version: R::VERSION,
        deps: &DependencyConstraints<P, R>,
    ) -> std::ops::Range<IncompId<P, R>> {
        // Create incompatibilities and allocate them in the store.
        let new_incompats_id_range = self
            .incompatibility_store
            .alloc_iter(deps.iter().map(|dep| {
                Incompatibility::from_dependency(package.clone(), version.clone(), dep)
            }));
        // Merge the newly created incompatibilities with the older ones.
        for id in IncompId::range_to_iter(new_incompats_id_range.clone()) {
            self.merge_incompatibility(id);
        }
        new_incompats_id_range
    }

    /// Check if an incompatibility is terminal.
    pub fn is_terminal(&self, incompatibility: &Incompatibility<P, R>) -> bool {
        incompatibility.is_terminal(&self.root_package, &self.root_version)
    }

    /// Unit propagation is the core mechanism of the solving algorithm.
    /// CF <https://github.com/dart-lang/pub/blob/master/doc/solver.md#unit-propagation>
    pub fn unit_propagation(&mut self, package: P) -> Result<(), PubGrubError<P, R>> {
        self.unit_propagation_buffer.clear();
        self.unit_propagation_buffer.push(package);
        while let Some(current_package) = self.unit_propagation_buffer.pop() {
            // Iterate over incompatibilities in reverse order
            // to evaluate first the newest incompatibilities.
            let mut conflict_id = None;
            // We only care about incompatibilities if it contains the current package.
            for &incompat_id in self.incompatibilities[&current_package].iter().rev() {
                if self.used_incompatibilities.contains(&incompat_id) {
                    continue;
                }
                let current_incompat = &self.incompatibility_store[incompat_id];
                match self.partial_solution.relation(current_incompat) {
                    // If the partial solution satisfies the incompatibility
                    // we must perform conflict resolution.
                    Relation::Satisfied => {
                        conflict_id = Some(incompat_id);
                        break;
                    }
                    Relation::AlmostSatisfied(package_almost) => {
                        self.unit_propagation_buffer.push(package_almost.clone());
                        self.used_incompatibilities.insert(incompat_id);
                        // Add (not term) to the partial solution with incompat as cause.
                        self.partial_solution.add_derivation(
                            package_almost,
                            incompat_id,
                            &self.incompatibility_store,
                        );
                    }
                    Relation::Contradicted(_) => {
                        self.used_incompatibilities.insert(incompat_id);
                    }
                    _ => {}
                }
            }
            if let Some(incompat_id) = conflict_id {
                let (package_almost, root_cause) = self.conflict_resolution(incompat_id)?;
                self.unit_propagation_buffer.clear();
                self.unit_propagation_buffer.push(package_almost.clone());
                self.used_incompatibilities.insert(root_cause);
                // Add to the partial solution with incompat as cause.
                self.partial_solution.add_derivation(
                    package_almost,
                    root_cause,
                    &self.incompatibility_store,
                );
            }
        }
        // If there are no more changed packages, unit propagation is done.
        Ok(())
    }

    /// Return the root cause and the backtracked model.
    /// CF <https://github.com/dart-lang/pub/blob/master/doc/solver.md#unit-propagation>
    fn conflict_resolution(
        &mut self,
        incompatibility: IncompId<P, R>,
    ) -> Result<(P, IncompId<P, R>), PubGrubError<P, R>> {
        let mut current_incompat_id = incompatibility;
        let mut current_incompat_changed = false;
        loop {
            if self.incompatibility_store[current_incompat_id]
                .is_terminal(&self.root_package, &self.root_version)
            {
                return Err(PubGrubError::NoSolution(
                    self.build_derivation_tree(current_incompat_id),
                ));
            } else {
                let (satisfier, satisfier_level, previous_satisfier_level) = self
                    .partial_solution
                    .find_satisfier_and_previous_satisfier_level(
                        &self.incompatibility_store[current_incompat_id],
                        &self.incompatibility_store,
                    );
                match satisfier.clone() {
                    Decision { package, .. } => {
                        self.backtrack(
                            current_incompat_id,
                            current_incompat_changed,
                            previous_satisfier_level,
                        );
                        return Ok((package, current_incompat_id));
                    }
                    Derivation { cause, package } => {
                        if previous_satisfier_level != satisfier_level {
                            self.backtrack(
                                current_incompat_id,
                                current_incompat_changed,
                                previous_satisfier_level,
                            );
                            return Ok((package, current_incompat_id));
                        } else {
                            let prior_cause = Incompatibility::prior_cause(
                                current_incompat_id,
                                cause,
                                &package,
                                &self.incompatibility_store,
                            );
                            current_incompat_id = self.incompatibility_store.alloc(prior_cause);
                            current_incompat_changed = true;
                        }
                    }
                }
            }
        }
    }

    /// Backtracking.
    fn backtrack(
        &mut self,
        incompat: IncompId<P, R>,
        incompat_changed: bool,
        decision_level: DecisionLevel,
    ) {
        self.partial_solution
            .backtrack(decision_level, &self.incompatibility_store);
        self.used_incompatibilities.clear();
        if incompat_changed {
            self.merge_incompatibility(incompat);
        }
    }

    /// Add this incompatibility into the set of all incompatibilities.
    ///
    /// Pub collapses identical dependencies from adjacent package versions
    /// into individual incompatibilities.
    /// This substantially reduces the total number of incompatibilities
    /// and makes it much easier for Pub to reason about multiple versions of packages at once.
    ///
    /// For example, rather than representing
    /// foo 1.0.0 depends on bar ^1.0.0 and
    /// foo 1.1.0 depends on bar ^1.0.0
    /// as two separate incompatibilities,
    /// they are collapsed together into the single incompatibility {foo ^1.0.0, not bar ^1.0.0}
    /// (provided that no other version of foo exists between 1.0.0 and 2.0.0).
    /// We could collapse them into { foo (1.0.0 ∪ 1.1.0), not bar ^1.0.0 }
    /// without having to check the existence of other versions though.
    ///
    /// Here we do the simple stupid thing of just growing the Vec.
    /// It may not be trivial since those incompatibilities
    /// may already have derived others.
    fn merge_incompatibility(&mut self, id: IncompId<P, R>) {
        for (pkg, _term) in self.incompatibility_store[id].iter() {
            self.incompatibilities
                .entry(pkg.clone())
                .or_default()
                .push(id);
        }
    }

    // Error reporting #########################################################

    fn build_derivation_tree(&self, incompat: IncompId<P, R>) -> DerivationTree<P, R> {
        let shared_ids = self.find_shared_ids(incompat);
        Incompatibility::build_derivation_tree(incompat, &shared_ids, &self.incompatibility_store)
    }

    fn find_shared_ids(&self, incompat: IncompId<P, R>) -> Set<IncompId<P, R>> {
        let mut all_ids = Set::new();
        let mut shared_ids = Set::new();
        let mut stack = vec![incompat];
        while let Some(i) = stack.pop() {
            if let Some((id1, id2)) = self.incompatibility_store[i].causes() {
                if all_ids.contains(&i) {
                    shared_ids.insert(i);
                } else {
                    all_ids.insert(i);
                    stack.push(id1);
                    stack.push(id2);
                }
            }
        }
        shared_ids
    }
}
