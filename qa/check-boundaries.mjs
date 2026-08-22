import { readFile } from "node:fs/promises";

const processPackages = new Set([
  "aworkit-trusted-core",
  "aworkit-workflow-worker",
  "aworkit-capability-host",
  "aworkit-local-store",
  "aworkit-portable-store",
  "aworkit-bootstrap-helper",
]);
const allowedInfrastructure = new Set(["aworkit-process", "aworkit-protocol"]);

function productionKind(kind) {
  return kind === "normal" || kind === "build" || kind === null;
}

export function violations(packages) {
  const problems = [];
  for (const pkg of packages) {
    for (const dependency of pkg.dependencies) {
      const internal = dependency.name.startsWith("aworkit-");
      const production = dependency.kinds.some(productionKind);
      if (
        processPackages.has(pkg.name) &&
        internal &&
        production &&
        !allowedInfrastructure.has(dependency.name)
      ) {
        problems.push(
          `${pkg.name} has production dependency on isolated process ${dependency.name}`,
        );
      }
      if (
        pkg.name === "aworkit-protocol" &&
        dependency.kinds.length > 0 &&
        (internal || dependency.name === "tauri" || dependency.name.startsWith("rig"))
      ) {
        problems.push(
          `aworkit-protocol depends on forbidden implementation ${dependency.name}`,
        );
      }
    }
  }
  return problems;
}

function normalizedMetadata(metadata) {
  const workspaceIds = new Set(metadata.workspace_members);
  const names = new Map(
    metadata.packages.map((pkg) => [pkg.id, pkg.name]),
  );
  return metadata.resolve.nodes
    .filter((node) => workspaceIds.has(node.id))
    .map((node) => ({
      name: names.get(node.id),
      dependencies: node.deps.map((dependency) => ({
        name: names.get(dependency.pkg) ?? dependency.name,
        kinds: dependency.dep_kinds.map(({ kind }) => kind ?? "normal"),
      })),
    }));
}

async function selfTest() {
  const root = new URL("./fixtures/", import.meta.url);
  const allowed = JSON.parse(
    await readFile(new URL("boundaries-allowed.json", root), "utf8"),
  );
  const forbidden = JSON.parse(
    await readFile(new URL("boundaries-forbidden.json", root), "utf8"),
  );
  const allowedProblems = violations(allowed.packages);
  if (allowedProblems.length > 0) {
    throw new Error(`allowed boundary fixture failed: ${allowedProblems.join("; ")}`);
  }
  const forbiddenProblems = violations(forbidden.packages);
  for (const expected of forbidden.expectedFragments) {
    if (!forbiddenProblems.some((problem) => problem.includes(expected))) {
      throw new Error(`forbidden boundary fixture did not catch ${expected}`);
    }
  }
}

if (process.argv.includes("--self-test")) {
  await selfTest();
} else if (process.argv.includes("--metadata-stdin")) {
  let input = "";
  process.stdin.setEncoding("utf8");
  for await (const chunk of process.stdin) input += chunk;
  const metadata = JSON.parse(input);
  const problems = violations(normalizedMetadata(metadata));
  if (problems.length > 0) {
    for (const problem of problems) console.error(problem);
    process.exitCode = 1;
  }
} else {
  throw new Error("expected --self-test or --metadata-stdin");
}
