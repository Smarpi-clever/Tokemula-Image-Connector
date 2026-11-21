// Restore only this package through the installed, version-matched native management APIs.
import { readFile, realpath } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { pathToFileURL } from 'node:url';
import { join } from 'node:path';

try {
  let text = '';
  for await (const chunk of process.stdin) text += chunk;
  const request = JSON.parse(text);
  const require = createRequire(await realpath(request.cli));
  const load = name => import(pathToFileURL(require.resolve(name)).href);
  const { INSTALL_ANCHOR } = await load('@deepseek-ai/dsh/profile-boot');
  const installed = JSON.parse(await readFile(INSTALL_ANCHOR, 'utf8'));
  if (installed.version !== '0.2.2-rc.5') throw new Error('unsupported runtime');
  const { runProfilePnpm, saveManifest } = await load('@deepseek-ai/dsh-plugin-manager/operations');
  const { readProfileManifest } = await load('@deepseek-ai/dsh-app-boot');
  const { withFileLock } = await load('@deepseek-ai/dsh-atomic-write');
  const name = 'tokemula-image-connector';
  const packageManifest = JSON.parse(await readFile(join(request.package_path, 'package.json'), 'utf8'));
  if (packageManifest.name !== name) throw new Error('unowned package');
  const config = join(request.profile_path, 'package.json');
  await readFile(config);
  await withFileLock(config, async () => {
    let manifest = readProfileManifest('dsh', request.profile_path);
    if (request.before.bundle_index === null && manifest.dsh.profile.bundles.includes(name)) {
      manifest.dsh.profile.bundles = manifest.dsh.profile.bundles.filter(value => value !== name);
      await saveManifest(request.profile_path, manifest);
    }
    const result = await runProfilePnpm({
      profile: request.profile, dir: request.profile_path,
      installAnchor: INSTALL_ANCHOR, cwd: request.cwd, home: request.dsh_home,
    }, ['add', request.package_path, '--workspace-root', '--offline', '--ignore-scripts'], {
      execution: 'service', outputBytes: 4096, activateNewBundles: false,
      expected: { name, version: packageManifest.version },
    });
    if (result.exitCode !== 0) throw new Error('native package recovery failed');
    manifest = readProfileManifest('dsh', request.profile_path);
    manifest.dependencies[name] = request.before.dependency;
    const bundles = manifest.dsh.profile.bundles.filter(value => value !== name);
    if (request.before.bundle_index !== null) {
      bundles.splice(Math.min(request.before.bundle_index, bundles.length), 0, name);
    }
    manifest.dsh.profile.bundles = bundles;
    await saveManifest(request.profile_path, manifest);
  });
} catch {
  process.stderr.write('DeepSeek native recovery did not complete.\n');
  process.exitCode = 1;
}
