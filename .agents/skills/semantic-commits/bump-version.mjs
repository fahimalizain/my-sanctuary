import { readFileSync, writeFileSync } from 'fs';
import { resolve } from 'path';

const pkgPath = resolve(process.cwd(), 'apps/web/package.json');
const pkg = JSON.parse(readFileSync(pkgPath, 'utf-8'));

const [major, minor, patch] = pkg.version.split('.').map(Number);
pkg.version = `${major}.${minor}.${patch + 1}`;

writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + '\n');
console.log(`🔖 bumped apps/web version to ${pkg.version}`);
