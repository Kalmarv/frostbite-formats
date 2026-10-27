//! The skeleton reader's checks on a skeleton by itself: a joint name is hashed with FNV-1, and the parents may not
//! lead back. The checks on the game's own skeleton (`animation/dingo/animbase_default_skeleton`, and the meshes
//! whose bone lists address it) are in primase's
//! [`worlds/frostbite/tests/skeleton.rs`](https://github.com/Kalmarv/primase/tree/main/worlds/frostbite/tests/skeleton.rs).

#![allow(clippy::unwrap_used)]

use frostbite_formats::Error;
use frostbite_formats::skeleton::{SkeletonJoint, name_hash, verify_hierarchy};

fn joint(name: &str, parent: Option<u32>) -> SkeletonJoint {
    SkeletonJoint {
        name: name.to_owned(),
        name_hash: name_hash(name),
        parent,
        child: None,
        sibling: None,
        kind: 1,
    }
}

#[test]
fn a_joint_name_is_hashed_with_fnv1_and_the_parents_may_not_lead_back() {
    // the hashes the skeleton asset of the game stores for three of its joints
    assert_eq!(name_hash("Reference"), 3_124_388_676);
    assert_eq!(name_hash("AITrajectory"), 1_180_344_196);
    assert_eq!(name_hash("Head"), 806_557_287);
    let chain = [joint("a", None), joint("b", Some(0)), joint("c", Some(1))];
    assert!(verify_hierarchy(&chain).is_ok());
    let cycle = [joint("a", Some(2)), joint("b", Some(0)), joint("c", Some(1))];
    let e = verify_hierarchy(&cycle).unwrap_err();
    assert!(e.to_string().contains("lead back"), "{e}");
    let past = [joint("a", None), joint("b", Some(5))];
    let e = verify_hierarchy(&past).unwrap_err();
    assert!(matches!(e, Error::Format { offset: 1, .. }), "{e}");
    assert!(
        e.to_string().contains("parent 5 is not one of the 2 joints"),
        "{e}"
    );
}
