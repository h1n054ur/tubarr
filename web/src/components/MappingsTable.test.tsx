import { render, screen, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { drama, podcast, theri } from '../test-data'
import { MappingsTable } from './MappingsTable'

const cells = (row: HTMLElement) => within(row).getAllByRole('cell')

describe('MappingsTable', () => {
  it('shows each mapping with its kind, ids, counts and status', () => {
    render(<MappingsTable mappings={[drama, theri]} onEdit={() => {}} />)
    const rows = screen.getAllByRole('row')
    expect(rows).toHaveLength(3)
    const show = within(rows[1])
    expect(show.getByText('Kabhi Main Kabhi Tum (2024)')).toBeInTheDocument()
    expect(show.getByText('TVDB 452001 · Urdu')).toBeInTheDocument()
    expect(show.getByText('12')).toBeInTheDocument()
    expect(cells(rows[1])[5]).toHaveTextContent('Active')
    const film = within(rows[2])
    expect(cells(rows[2])[1]).toHaveTextContent('Film')
    expect(film.getByText('TMDB 352173 · Hindi')).toBeInTheDocument()
    expect(film.getByText('none set')).toBeInTheDocument()
    expect(cells(rows[2])[5]).toHaveTextContent('Inactive')
  })

  it('repeats kind and status under the title for phones', () => {
    render(<MappingsTable mappings={[drama, podcast, theri]} onEdit={() => {}} />)
    const rows = screen.getAllByRole('row')
    const summary = (row: HTMLElement) => {
      const first = within(cells(row)[0])
      expect(first.getAllByRole('term').map((t) => t.textContent)).toEqual(['Kind', 'Status'])
      return first.getAllByRole('definition').map((d) => d.textContent)
    }
    expect(summary(rows[1])).toEqual(['Show', 'Active'])
    expect(summary(rows[2])).toEqual(['Podcast', 'Active'])
    expect(summary(rows[3])).toEqual(['Film', 'Inactive'])
    expect(screen.getByRole('columnheader', { name: 'Kind' })).toHaveClass('hidden', 'sm:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Channels' })).toHaveClass('hidden', 'md:table-cell')
    expect(screen.getByRole('columnheader', { name: 'Edit' })).not.toHaveClass('hidden')
  })

  it('marks a podcast with a Podcast badge in place of Show', () => {
    render(<MappingsTable mappings={[drama, podcast]} onEdit={() => {}} />)
    const rows = screen.getAllByRole('row')
    expect(cells(rows[1])[1]).toHaveTextContent('Show')
    expect(within(rows[1]).queryByText('Podcast')).not.toBeInTheDocument()
    const badge = within(cells(rows[2])[1]).getByText('Podcast')
    expect(badge).toHaveClass('ring-1')
    expect(within(rows[2]).queryByText('Show')).not.toBeInTheDocument()
  })

  it('opens the editor for the clicked row', async () => {
    const onEdit = vi.fn()
    render(<MappingsTable mappings={[drama, theri]} onEdit={onEdit} />)
    await userEvent.click(screen.getByRole('button', { name: /Edit.*Theri/ }))
    expect(onEdit).toHaveBeenCalledWith(theri)
  })

  it('opens the editor from a click anywhere on the row, once', async () => {
    const onEdit = vi.fn()
    render(<MappingsTable mappings={[drama, theri]} onEdit={onEdit} />)
    await userEvent.click(screen.getByText('TVDB 452001 · Urdu'))
    expect(onEdit).toHaveBeenCalledWith(drama)
    await userEvent.click(screen.getByRole('button', { name: /Edit.*Theri/ }))
    expect(onEdit).toHaveBeenCalledTimes(2)
  })
})
