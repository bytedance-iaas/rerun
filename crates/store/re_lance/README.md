# re_lance

Part of the [`rerun`](https://github.com/rerun-io/rerun) family of crates.

Reads Lance-format `LeRobot` datasets (as produced by `lerobot-lancedb` or published in the
`lance-format` three-table layout) by presenting them as a virtual `LeRobot` file system
(`re_lerobot::vfs::LeRobotFs`), so the existing v2/v3 dataset-to-chunk conversion code can consume
them unchanged.

Native-only: the `lance` crate does not compile to wasm.
