cargo build --workspace --release --locked && (cd desktop && pnpm exec tauri build --no-bundle)
pause