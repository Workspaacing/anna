# Cli

## Testing

You can test your changes to the `cli` crate by first building the main `anna` binary:

```
cargo build --bin anna
```

And then building and running the `cli` crate with the following parameters:

```
 cargo run -p cli -- --zed ./target/debug/anna.exe
```
