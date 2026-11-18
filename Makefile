# make        -> builds ./server (Rust, release)
# make clean  -> removes build output
all: server

server:
	cargo build --release --manifest-path rust_server/Cargo.toml
	cp rust_server/target/release/zappy_server ./server

test:
	cargo test --manifest-path rust_server/Cargo.toml

clean:
	cargo clean --manifest-path rust_server/Cargo.toml
	rm -f ./server

.PHONY: all server test clean
