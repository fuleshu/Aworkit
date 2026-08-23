import { readFileSync } from "node:fs";

const matrixPath = process.argv[2] ?? "qa/fixtures/milestone-12-platform-matrix.json";
const manifestPath = process.argv[3];
const matrix = JSON.parse(readFileSync(matrixPath, "utf8"));

assert(matrix.schemaVersion === 1, "matrix schema version");
assert(
  JSON.stringify(matrix.platforms.map(({ id }) => id).sort()) ===
    JSON.stringify(["linux", "macos", "windows"]),
  "exact Windows/macOS/Linux matrix",
);
const expectedDegraded = [
  "credential-store-locked",
  "native-presentation-unavailable-workbench-fallback",
  "non-local-or-package-owned-volume",
  "packaged-self-activation-unsupported",
  "process-containment-unavailable",
];
for (const platform of matrix.platforms) {
  for (const boundary of ["filesystem", "process", "credential", "presentation"])
    assert(typeof platform[boundary] === "string" && platform[boundary] !== "", `${platform.id} ${boundary}`);
  assert(
    JSON.stringify([...platform.degradedStates].sort()) ===
      JSON.stringify(expectedDegraded),
    `${platform.id} degraded states`,
  );
}
assert(
  JSON.stringify([...matrix.requiredBundleRoles].sort()) ===
    JSON.stringify([
      "bootstrap_helper",
      "capability_host",
      "desktop",
      "desktop_ui",
      "trusted_core",
      "workflow_worker",
    ]),
  "whole-application bundle roles",
);

if (manifestPath !== undefined) {
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const roles = [...new Set(manifest.entries.map(({ role }) => role))].sort();
  assert(JSON.stringify(roles) === JSON.stringify(matrix.requiredBundleRoles), "assembled roles");
  assert(manifest.protocolCompatibility.repairBootstrapSchema === 1, "repair protocol");
  assert(manifest.protocolCompatibility.extensionHostProtocol === 1, "host protocol");
  assert(manifest.buildOrigin.kind === "packaged_distribution", "packaged origin");
  assert(
    manifest.managedLocalSelfActivation === "packaged_distribution",
    "packaged managed-local activation is explicitly unavailable",
  );
  for (const field of [
    "sourceTreeHash",
    "workspaceIdentityHash",
    "toolchainHash",
    "buildManifestHash",
    "provenanceHash",
  ])
    assert(/^sha256:[0-9a-f]{64}$/.test(manifest.provenance[field]), `provenance ${field}`);
}

console.log("milestone-12 platform and release matrix: conformant");

function assert(condition, label) {
  if (!condition) throw new Error(`Milestone 12 conformance failed: ${label}`);
}
