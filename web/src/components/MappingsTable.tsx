// Rows of the in-card table from ./Table (Tailwind Plus simple-in-card). A click anywhere on a row
// opens the editor; the Edit button stays as the keyboard way in. On phones Kind and Status move under
// the title and the counts show from md up (see Stacked in ./Table), so Edit never leaves the screen.
import type { ReactNode } from 'react'
import type { Mapping } from '../lib/api'
import { Badge } from './Badge'
import { Stacked, Table, Td, Th } from './Table'

export const kindLabel = { show: 'Show', daily: 'Daily show', film: 'Film' } as const

function ids(m: Mapping) {
  if (m.kind === 'film') return m.tmdb_id ? `TMDB ${m.tmdb_id}` : ''
  return m.tvdb_id ? `TVDB ${m.tvdb_id}` : ''
}

export function MappingsTable({
  mappings,
  onEdit,
  footer,
}: {
  mappings: Mapping[]
  onEdit: (m: Mapping) => void
  footer?: ReactNode
}) {
  return (
    <Table
      footer={footer}
      head={
        <>
          <Th first>Title</Th>
          <Th hideBelow="sm">Kind</Th>
          <Th hideBelow="md">Channels</Th>
          <Th hideBelow="md">Releases</Th>
          <Th hideBelow="md">Unmatched</Th>
          <Th hideBelow="sm">Status</Th>
          <Th last>
            <span className="sr-only">Edit</span>
          </Th>
        </>
      }
    >
      {mappings.map((m) => {
        const kind = m.is_podcast ? <Badge tone="cyan">Podcast</Badge> : kindLabel[m.kind]
        const status = m.active ? <Badge tone="green">Active</Badge> : <Badge tone="gray">Inactive</Badge>
        return (
          <tr key={m.id} onClick={() => onEdit(m)} className="cursor-pointer hover:bg-ink/5">
            <Td first stackBelow="sm">
              <div className="font-medium text-ink">
                {m.title}
                {m.year ? ` (${m.year})` : ''}
              </div>
              <div className="mt-1 text-muted">
                {ids(m)} · {m.language}
              </div>
              <Stacked
                below="sm"
                items={[
                  { label: 'Kind', value: kind },
                  { label: 'Status', value: status },
                ]}
              />
            </Td>
            <Td hideBelow="sm">{kind}</Td>
            <Td hideBelow="md">
              {m.channel_ids.length === 0 ? <Badge tone="yellow">none set</Badge> : m.channel_ids.length}
            </Td>
            <Td hideBelow="md">{m.releases}</Td>
            <Td hideBelow="md">{m.unmatched}</Td>
            <Td hideBelow="sm">{status}</Td>
            <td className="py-4 pr-4 pl-3 text-right text-sm font-medium whitespace-nowrap sm:pr-6">
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation()
                  onEdit(m)
                }}
                className="text-brand-text hover:text-brand-dark"
              >
                Edit<span className="sr-only">, {m.title}</span>
              </button>
            </td>
          </tr>
        )
      })}
    </Table>
  )
}
