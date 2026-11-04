import { act, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { SearchBox } from './SearchBox'

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

function box(value = '') {
  const onChange = vi.fn()
  const view = render(<SearchBox id="search" label="Search shows" value={value} onChange={onChange} />)
  return { onChange, ...view }
}

describe('SearchBox', () => {
  it('sends the text once typing pauses', () => {
    const { onChange } = box()
    const input = screen.getByLabelText('Search shows')
    fireEvent.change(input, { target: { value: 'ro' } })
    act(() => vi.advanceTimersByTime(100))
    fireEvent.change(input, { target: { value: 'rogan' } })
    act(() => vi.advanceTimersByTime(100))
    expect(onChange).not.toHaveBeenCalled()
    act(() => vi.advanceTimersByTime(60))
    expect(onChange).toHaveBeenCalledTimes(1)
    expect(onChange).toHaveBeenCalledWith('rogan')
  })

  it('clears at once and keeps the focus in the box', () => {
    const { onChange } = box('rogan')
    const input = screen.getByLabelText('Search shows')
    expect(input).toHaveValue('rogan')
    fireEvent.click(screen.getByRole('button', { name: 'Clear search' }))
    expect(onChange).toHaveBeenCalledWith('')
    expect(input).toHaveValue('')
    expect(input).toHaveFocus()
    expect(screen.queryByRole('button', { name: 'Clear search' })).not.toBeInTheDocument()
  })

  it('focuses on / unless another field has the keyboard', () => {
    render(
      <>
        <input aria-label="Other" />
        <SearchBox id="search" label="Search shows" value="" onChange={() => {}} />
      </>,
    )
    const input = screen.getByLabelText('Search shows')
    const other = screen.getByLabelText('Other')
    other.focus()
    fireEvent.keyDown(other, { key: '/' })
    expect(other).toHaveFocus()
    other.blur()
    const press = fireEvent.keyDown(document.body, { key: '/' })
    expect(input).toHaveFocus()
    expect(press).toBe(false)
  })

  it('takes a new value from outside', () => {
    const { rerender } = box('rogan')
    rerender(<SearchBox id="search" label="Search shows" value="theo" onChange={() => {}} />)
    expect(screen.getByLabelText('Search shows')).toHaveValue('theo')
  })

  it('does not undo typing in flight when the parent echoes the sent value', () => {
    const onChange = vi.fn()
    const { rerender } = render(<SearchBox id="search" label="Search shows" value="" onChange={onChange} />)
    const input = screen.getByLabelText('Search shows')
    fireEvent.change(input, { target: { value: 'rog' } })
    act(() => vi.advanceTimersByTime(150))
    fireEvent.change(input, { target: { value: 'roga' } })
    rerender(<SearchBox id="search" label="Search shows" value="rog" onChange={onChange} />)
    expect(input).toHaveValue('roga')
  })
})
