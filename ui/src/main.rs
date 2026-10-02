mod api;
mod app;
mod components;
// Test-only, and only under `cargo test`: `trunk build` never compiles it.
// See `tests/harness.rs` for what the harness does and does not cover.
#[cfg(test)]
mod tests;

fn main() {
    yew::Renderer::<app::App>::new().render();
}
