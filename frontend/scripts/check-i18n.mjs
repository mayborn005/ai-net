import { readdir, readFile } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const localesDir = fileURLToPath(new URL('../src/i18n/locales/', import.meta.url))
const flattenKeys = (value, prefix = '') => Object.entries(value).flatMap(([key, child]) => {
  const fullKey = prefix ? `${prefix}.${key}` : key
  return child && typeof child === 'object' && !Array.isArray(child)
    ? flattenKeys(child, fullKey)
    : [fullKey]
}).sort()

const files = (await readdir(localesDir)).filter((file) => file.endsWith('.json')).sort()
const localeKeys = new Map()
for (const file of files) {
  localeKeys.set(file, flattenKeys(JSON.parse(await readFile(join(localesDir, file), 'utf8'))))
}

const baseFile = 'en.json'
const baseKeys = localeKeys.get(baseFile)
if (!baseKeys) throw new Error(`Missing base locale: ${baseFile}`)

let failed = false
for (const [file, keys] of localeKeys) {
  if (file === baseFile) continue
  const missing = baseKeys.filter((key) => !keys.includes(key))
  const extra = keys.filter((key) => !baseKeys.includes(key))
  if (missing.length || extra.length) {
    failed = true
    console.error(`${file}: missing [${missing.join(', ')}]; extra [${extra.join(', ')}]`)
  }
}

if (failed) process.exit(1)
console.log(`i18n parity check passed for ${files.join(', ')}`)
