// Run with the installed @iconify-json/twemoji directory as the only argument.
import { readFileSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { gzipSync } from 'node:zlib'

const directory = process.argv[2]
const read = (name) => JSON.parse(readFileSync(join(directory, name), 'utf8'))
const { version } = read('package.json')
if (version !== '1.2.5') throw new Error(`Expected Twemoji 1.2.5, got ${version}`)

const collection = read('icons.json')
const assets = Object.fromEntries(Object.entries(read('chars.json')).map(([code, name]) => {
  const icon = collection.icons[name]
  const width = icon.width ?? collection.width
  const height = icon.height ?? collection.height
  return [`twemoji/${code}.svg`, `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${width} ${height}">${icon.body}</svg>`]
}))
writeFileSync(new URL('../assets/emoji/twemoji-1.2.5.json.gz', import.meta.url),
  gzipSync(JSON.stringify(assets), { level: 9 }))
