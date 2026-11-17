// Starts a real Visual Studio Code with the built extension and a real
// `meridian` binary, and runs suite.ts inside it.
//
//   MERIDIAN_BIN=/path/to/meridian npm run test:e2e
//
// The binary's directory must also hold `meridian-target-ansible`, which
// `cargo build -p meridian-cli -p meridian-target-ansible` arranges.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { runTests } from "@vscode/test-electron";

const DOCUMENT = `apiVersion: meridian/v1
kind: ResourceSet
metadata:
  name: web
spec:
  target: ansible
  targetHost:
    from: inline
    host: web01.example.com
  resources:
    - id: nginx
      type: package
      params:
        name: nginx
    - id: site
      type: file
      params:
        path: /etc/nginx/conf.d/site.conf
        mode: 0644
        colour: blue
      dependsOn:
          - resource: nginx
`;

async function main(): Promise<void> {
  const binary = process.env.MERIDIAN_BIN;
  if (binary === undefined || !fs.existsSync(binary)) {
    throw new Error("set MERIDIAN_BIN to a built `meridian` binary");
  }
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "meridian-vscode-"));
  fs.mkdirSync(path.join(workspace, ".vscode"));
  fs.writeFileSync(
    path.join(workspace, ".vscode", "settings.json"),
    JSON.stringify({ "meridian.path": path.resolve(binary) }),
  );
  fs.writeFileSync(path.join(workspace, "web.yaml"), DOCUMENT);
  // A YAML file that is not a Meridian document, which must be left alone.
  fs.writeFileSync(path.join(workspace, "other.yaml"), "on:\n    push: {}\n");

  await runTests({
    extensionDevelopmentPath: path.resolve(__dirname, ".."),
    extensionTestsPath: path.resolve(__dirname, "suite.js"),
    launchArgs: [workspace, "--disable-extensions", "--disable-workspace-trust"],
  });
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
