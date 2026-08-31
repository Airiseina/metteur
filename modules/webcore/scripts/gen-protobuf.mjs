// Regenerates TypeScript bindings from the canonical `metteur.proto`.
//
// Uses `protoc` with the Protobuf-ES plugin (`@bufbuild/protoc-gen-es`). With
// Protobuf-ES v2 the generated `.pb.ts` already carries the typed `Daemon`
// service descriptor (`createClient` consumes it directly), so a separate
// connect plugin is not needed. Generated files live under `src/gen` and are
// committed so frontend builds never depend on `protoc` at runtime.
import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'

const root = dirname(dirname(fileURLToPath(import.meta.url)))
const binDir = join(root, 'node_modules', '.bin')
const maybeBin = (name) =>
  process.platform === 'win32' ? join(binDir, `${name}.CMD`) : join(binDir, name)

// Locate the plugin explicitly so `protoc` does not need it on PATH.
const protocGenEs = maybeBin('protoc-gen-es')

// The single source of truth is the workspace-level schema directory.
const schemaDir = join(root, '..', '..', 'schema', 'proto')
const outDir = join(root, 'src', 'gen')

execFileSync(
  'protoc',
  [
    `--proto_path=${schemaDir}`,
    `--plugin=protoc-gen-es=${protocGenEs}`,
    `--es_out=${outDir}`,
    `--es_opt=target=ts`,
    'metteur.proto',
  ],
  { stdio: 'inherit' },
)

console.log(`generated protobuf bindings -> ${outDir}`)