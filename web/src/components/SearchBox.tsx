// Ported from Tailwind Plus application-ui/forms/input-groups/input-with-leading-icon, with a clear
// button in the trailing slot, recoloured with the Tubarr tokens. `/` anywhere on the page focuses it.
import { MagnifyingGlassIcon, XMarkIcon } from '@heroicons/react/16/solid'
import { useEffect, useRef, useState } from 'react'

function typingElsewhere(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false
  return target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)
}

/** `onChange` fires `delay` ms after the last keystroke, and at once on clear. A new `value` from
 * outside (back button, a link) replaces the text in the box. */
export function SearchBox({
  id,
  label,
  value,
  onChange,
  placeholder = 'Search',
  hideLabel = false,
  delay = 150,
}: {
  id: string
  label: string
  value: string
  onChange: (value: string) => void
  placeholder?: string
  hideLabel?: boolean
  delay?: number
}) {
  const [text, setText] = useState(value)
  const sent = useRef(value)
  const input = useRef<HTMLInputElement>(null)
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined)

  useEffect(() => {
    if (value !== sent.current) {
      sent.current = value
      setText(value)
    }
  }, [value])

  useEffect(() => () => clearTimeout(timer.current), [])

  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey || typingElsewhere(e.target)) return
      e.preventDefault()
      input.current?.focus()
    }
    document.addEventListener('keydown', onKey)
    return () => document.removeEventListener('keydown', onKey)
  }, [])

  function send(next: string) {
    clearTimeout(timer.current)
    sent.current = next
    onChange(next)
  }

  function type(next: string) {
    setText(next)
    clearTimeout(timer.current)
    timer.current = setTimeout(() => send(next), delay)
  }

  function clear() {
    setText('')
    send('')
    input.current?.focus()
  }

  return (
    <div>
      <label htmlFor={id} className={hideLabel ? 'sr-only' : 'block text-sm/6 font-medium text-ink'}>
        {label}
      </label>
      <div className={`${hideLabel ? '' : 'mt-2 '}grid grid-cols-1`}>
        <input
          ref={input}
          id={id}
          type="search"
          autoComplete="off"
          spellCheck={false}
          placeholder={placeholder}
          aria-keyshortcuts="/"
          value={text}
          onChange={(e) => type(e.target.value)}
          className="col-start-1 row-start-1 block w-full rounded-md bg-surface py-1.5 pr-9 pl-10 text-base text-ink outline-1 -outline-offset-1 outline-ink/20 placeholder:text-muted focus:outline-2 focus:-outline-offset-2 focus:outline-brand sm:pl-9 sm:text-sm/6 [&::-webkit-search-cancel-button]:hidden"
        />
        <MagnifyingGlassIcon
          aria-hidden="true"
          className="pointer-events-none col-start-1 row-start-1 ml-3 size-5 self-center text-muted sm:size-4"
        />
        {text && (
          <button
            type="button"
            onClick={clear}
            className="col-start-1 row-start-1 mr-2 self-center justify-self-end rounded-sm p-0.5 text-muted hover:text-ink focus-visible:outline-2 focus-visible:outline-brand"
          >
            <span className="sr-only">Clear search</span>
            <XMarkIcon aria-hidden="true" className="size-5 sm:size-4" />
          </button>
        )}
      </div>
    </div>
  )
}
