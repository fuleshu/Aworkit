From the repository root:
cd /home/timofl/src/Aworkit

# Build the framework
cargo build --workspace --release --locked

# Build the desktop app
cd desktop
pnpm install --frozen-lockfile
pnpm desktop:build
Outputs:
- Framework: target/release/
- Desktop executable: desktop/src-tauri/target/release/aworkit-desktop
- Installable packages: desktop/src-tauri/target/release/bundle/

Run the built app:
./desktop/src-tauri/target/release/aworkit-desktop

For development mode:
cd desktop
pnpm desktop:dev