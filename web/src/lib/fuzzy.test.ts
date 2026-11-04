import { describe, expect, it } from 'vitest'
import { fuzzySearch, normalise, type FuzzyKey } from './fuzzy'

interface Row {
  title: string
  aliases?: string[]
  id: number
  language: string
}

const titles: [string, string][] = [
  ['The Joe Rogan Experience', 'English'],
  ['This Past Weekend w/Theo Von', 'English'],
  ['Kabhi Main Kabhi Tum', 'Urdu'],
  ['Tere Bin', 'Urdu'],
  ['Mere Humsafar', 'Urdu'],
  ['Jaan Nisar', 'Urdu'],
  ['Ishq Murshid', 'Urdu'],
  ['Kaffara', 'Urdu'],
  ['Bismil', 'Urdu'],
  ['Mann Mayal', 'Urdu'],
  ['Humsafar', 'Urdu'],
  ['Zindagi Gulzar Hai', 'Urdu'],
  ['Theri', 'Tamil'],
  ['Anupamaa', 'Hindi'],
  ['Yeh Rishta Kya Kehlata Hai', 'Hindi'],
  ['Kumkum Bhagya', 'Hindi'],
  ['Bhabiji Ghar Par Hain!', 'Hindi'],
  ['Taarak Mehta Ka Ooltah Chashmah', 'Hindi'],
  ['The Kapil Sharma Show', 'Hindi'],
  ['Shark Tank India', 'Hindi'],
  ['Lex Fridman Podcast', 'English'],
  ['Huberman Lab', 'English'],
  ['Hot Ones', 'English'],
  ['Last Week Tonight with John Oliver', 'English'],
  ['The Daily Show', 'English'],
  ['A Café in Montréal', 'English'],
  ['An Idiot Abroad', 'English'],
  ['Call Her Daddy', 'English'],
  ['SmartLess', 'English'],
  ['कौन बनेगा करोड़पति', 'Hindi'],
  ['میرے ہمسفر', 'Urdu'],
]

const rows: Row[] = titles.map(([title, language], i) => ({ title, language, id: 400000 + i }))
// A title with digits in it, to check an id search still finds titles.
rows.push({ title: 'Studio 400012 Live', language: 'English', id: 999 })
rows[2].aliases = ['KMKT']

const keys: FuzzyKey<Row>[] = [
  { name: 'title', get: (r) => r.title, weight: 3 },
  { name: 'aliases', get: (r) => r.aliases, weight: 2 },
  { name: 'id', get: (r) => r.id, exact: true },
  { name: 'language', get: (r) => r.language, weight: 0.5 },
]

const search = (q: string) => fuzzySearch(rows, q, keys).map((r) => r.title)

describe('normalise', () => {
  it('drops case, accents, punctuation and a leading article', () => {
    expect(normalise('The Joe Rogan Experience')).toBe('joe rogan experience')
    expect(normalise('A Café in Montréal')).toBe('cafe in montreal')
    expect(normalise('This Past Weekend w/Theo Von')).toBe('this past weekend w theo von')
    expect(normalise("Rogan's  Show!")).toBe('rogans show')
    expect(normalise('Theri')).toBe('theri')
    expect(normalise('Anupamaa')).toBe('anupamaa')
  })

  it('keeps the vowel marks of other scripts', () => {
    expect(normalise('कौन बनेगा')).toBe('कौन बनेगा'.normalize('NFD'))
  })
})

describe('fuzzySearch', () => {
  it('returns the items unchanged for an empty or blank query', () => {
    expect(fuzzySearch(rows, '', keys)).toEqual(rows)
    expect(fuzzySearch(rows, '  ', keys)).toEqual(rows)
  })

  it('finds a show through typos, a partial name or a name inside the title', () => {
    expect(search('joe rogn')[0]).toBe('The Joe Rogan Experience')
    expect(search('rogan')[0]).toBe('The Joe Rogan Experience')
    expect(search('theo von')[0]).toBe('This Past Weekend w/Theo Von')
    expect(search('the joe rogan experience')[0]).toBe('The Joe Rogan Experience')
  })

  it('does not flood the results with unrelated titles', () => {
    expect(search('joe rogn')).toHaveLength(1)
    expect(search('rogan')).toHaveLength(1)
    expect(search('theo von')).toHaveLength(1)
    expect(search('huberman').length).toBeLessThanOrEqual(2)
  })

  it('ignores accents and articles in the query and the titles', () => {
    expect(search('cafe montreal')[0]).toBe('A Café in Montréal')
    expect(search('idiot abroad')[0]).toBe('An Idiot Abroad')
  })

  it('ranks the closer title first', () => {
    expect(search('humsafar').slice(0, 2)).toEqual(['Humsafar', 'Mere Humsafar'])
  })

  it('matches aliases, ids, language and non-Latin titles', () => {
    expect(search('kmkt')[0]).toBe('Kabhi Main Kabhi Tum')
    expect(search('400012')[0]).toBe('Theri')
    expect(search('tamil')).toEqual(['Theri'])
    expect(search('करोड़पति')[0]).toBe('कौन बनेगा करोड़पति')
    expect(search('ہمسفر')[0]).toBe('میرے ہمسفر')
  })

  it('finds nothing for an unrelated query', () => {
    expect(search('xyzzy')).toEqual([])
  })
})

describe('fuzzySearch word order', () => {
  it('matches every word of a query in any order', () => {
    expect(search('rogan joe')).toEqual(['The Joe Rogan Experience'])
    expect(search('von theo')).toEqual(['This Past Weekend w/Theo Von'])
    expect(search('montreal cafe')[0]).toBe('A Café in Montréal')
  })

  it('tolerates a typo in a reordered word', () => {
    expect(search('rogn joe')).toEqual(['The Joe Rogan Experience'])
  })

  it('lets the words match in different keys', () => {
    expect(search('theri tamil')).toEqual(['Theri'])
    expect(search('kmkt urdu')[0]).toBe('Kabhi Main Kabhi Tum')
  })

  it('needs every word to match', () => {
    expect(search('rogan xyzzy')).toEqual([])
    expect(search('rogan tamil')).toEqual([])
  })
})

describe('fuzzySearch exact ids', () => {
  it('puts the exact id first and leaves near ids out', () => {
    const found = search('400012')
    expect(found[0]).toBe('Theri')
    for (const near of ['Zindagi Gulzar Hai', 'Anupamaa']) expect(found).not.toContain(near)
  })

  it('still matches digits inside a title, after the exact id', () => {
    expect(search('400012')).toEqual(['Theri', 'Studio 400012 Live'])
  })

  it('finds no id for a partial number', () => {
    expect(search('40001')).not.toContain('Kabhi Main Kabhi Tum')
    expect(search('999')).toEqual(['Studio 400012 Live'])
  })

  it('counts an id as one word of a longer query', () => {
    expect(search('theri 400012')[0]).toBe('Theri')
    expect(search('tamil 400012')[0]).toBe('Theri')
  })
})
