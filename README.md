fast-rustup
===========

Experiment with a different architecture for Rustup that can perform downloads
and installation concurrently.

This is in contrast with Rustup, which downloads one component at a time and
then unpacks one component at a time. See [rustup#731].

[rustup#731]: https://github.com/rust-lang/rustup/issues/731

**rustup:**

```console
$ rustup toolchain remove nightly-2024-01-01
$ time rustup toolchain install nightly-2024-01-01
17.9 seconds
```

**fast-rustup:**

```console
$ git clone https://github.com/dtolnay/fast-rustup
$ cd fast-rustup
$ cargo build --release
$ rustup toolchain remove nightly-2024-01-01
$ time target/release/fast-rustup nightly-2024-01-01
5.4 seconds
```

<br>

#### License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
