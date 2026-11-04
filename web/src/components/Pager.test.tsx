import { render, screen } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { Pager, pageItems } from './Pager'

function pager(props: { page?: number; size?: 25 | 50 | 100; total: number }) {
  const onPage = vi.fn()
  const onSize = vi.fn()
  const { container } = render(
    <Pager page={props.page ?? 1} size={props.size ?? 25} total={props.total} onPage={onPage} onSize={onSize} />,
  )
  return { onPage, onSize, container }
}

describe('pageItems', () => {
  it('keeps the ends and the neighbours, with gaps in between', () => {
    expect(pageItems(1, 1)).toEqual([1])
    expect(pageItems(1, 3)).toEqual([1, 2, 3])
    expect(pageItems(1, 10)).toEqual([1, 2, 'gap', 10])
    expect(pageItems(5, 10)).toEqual([1, 'gap', 4, 5, 6, 'gap', 10])
    expect(pageItems(10, 10)).toEqual([1, 'gap', 9, 10])
  })

  it('shows a lone hidden page instead of a gap', () => {
    expect(pageItems(4, 10)).toEqual([1, 2, 3, 4, 5, 'gap', 10])
    expect(pageItems(7, 10)).toEqual([1, 'gap', 6, 7, 8, 9, 10])
  })
})

describe('Pager', () => {
  it('shows nothing when every row fits the smallest page', () => {
    expect(pager({ total: 25 }).container).toBeEmptyDOMElement()
    expect(pager({ total: 0 }).container).toBeEmptyDOMElement()
  })

  it('shows the range and moves between pages', async () => {
    const { onPage } = pager({ page: 2, total: 144 })
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 26 to 50 of 144')
    expect(screen.getByRole('button', { name: 'Page 2' })).toHaveAttribute('aria-current', 'page')
    await userEvent.click(screen.getByRole('button', { name: 'Next page' }))
    expect(onPage).toHaveBeenLastCalledWith(3)
    await userEvent.click(screen.getByRole('button', { name: 'Previous page' }))
    expect(onPage).toHaveBeenLastCalledWith(1)
    await userEvent.click(screen.getByRole('button', { name: 'Page 6' }))
    expect(onPage).toHaveBeenLastCalledWith(6)
  })

  it('disables the arrows at the ends', () => {
    pager({ page: 1, total: 60 })
    expect(screen.getByRole('button', { name: 'Previous page' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Next page' })).toBeEnabled()
    expect(screen.getByRole('button', { name: 'Previous' })).toBeDisabled()
  })

  it('ends the last page at the total and clamps a page past the end', () => {
    pager({ page: 9, total: 144 })
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 126 to 144 of 144')
    expect(screen.getByRole('button', { name: 'Next page' })).toBeDisabled()
  })

  it('changes the page size', async () => {
    const { onSize } = pager({ total: 144 })
    await userEvent.selectOptions(screen.getByLabelText('Per page'), '50')
    expect(onSize).toHaveBeenCalledWith(50)
  })

  it('keeps the Per page label on one line', () => {
    pager({ total: 144 })
    expect(screen.getByText('Per page')).toHaveClass('whitespace-nowrap')
  })

  it('keeps the size choice but no page buttons when one page holds everything', () => {
    pager({ size: 50, total: 40 })
    expect(screen.getByLabelText('Per page')).toHaveValue('50')
    expect(screen.getByText(/Showing/).textContent).toBe('Showing 1 to 40 of 40')
    expect(screen.queryByRole('navigation')).not.toBeInTheDocument()
  })
})
