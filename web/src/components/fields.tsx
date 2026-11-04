// Form controls ported from Tailwind Plus application-ui/forms (input-with-label-and-help-text,
// select-menus/simple-native, toggles/with-left-label-and-description), recoloured with the Tubarr tokens. The outline is ink at 20%, not the
// border token, so fields stay visible on the light theme's near-white surfaces.
import { ChevronDownIcon } from '@heroicons/react/16/solid'
import type { ReactNode } from 'react'
import { timeZones } from '../lib/podcast'

export const inputClass =
  'block w-full rounded-md bg-surface px-3 py-1.5 text-base text-ink outline-1 -outline-offset-1 outline-ink/20 placeholder:text-muted focus:outline-2 focus:-outline-offset-2 focus:outline-brand sm:text-sm/6'

/** `corner` sits opposite the label, as in Tailwind Plus input-with-corner-hint. */
export function Field({
  id,
  label,
  help,
  corner,
  children,
}: {
  id: string
  label: string
  help?: string
  corner?: ReactNode
  children: ReactNode
}) {
  return (
    <div>
      <div className="flex items-center justify-between gap-x-2">
        <label htmlFor={id} className="block text-sm/6 font-medium text-ink">
          {label}
        </label>
        {corner}
      </div>
      <div className="mt-2">{children}</div>
      {help && (
        <p id={`${id}-help`} className="mt-2 text-sm text-muted">
          {help}
        </p>
      )}
    </div>
  )
}

/** A text input that suggests the common time zones and takes any other IANA name typed in. */
export function TimeZoneInput({
  id,
  value,
  onChange,
  required,
  placeholder,
}: {
  id: string
  value: string
  onChange: (value: string) => void
  required?: boolean
  placeholder?: string
}) {
  return (
    <>
      <input
        id={id}
        list={`${id}-zones`}
        autoComplete="off"
        spellCheck={false}
        required={required}
        placeholder={placeholder}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={inputClass}
      />
      <datalist id={`${id}-zones`}>
        {timeZones.map((zone) => (
          <option key={zone} value={zone} />
        ))}
      </datalist>
    </>
  )
}

export function Select({
  id,
  label,
  value,
  onChange,
  options,
}: {
  id: string
  label: string
  value: string
  onChange: (value: string) => void
  options: { value: string; label: string }[]
}) {
  return (
    <div>
      <label htmlFor={id} className="block text-sm/6 font-medium text-ink">
        {label}
      </label>
      <div className="mt-2 grid grid-cols-1">
        <select
          id={id}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          className="col-start-1 row-start-1 w-full appearance-none rounded-md bg-surface py-1.5 pr-8 pl-3 text-base text-ink outline-1 -outline-offset-1 outline-ink/20 focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-brand sm:text-sm/6"
        >
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
        <ChevronDownIcon
          aria-hidden="true"
          className="pointer-events-none col-start-1 row-start-1 mr-2 size-5 self-center justify-self-end text-muted sm:size-4"
        />
      </div>
    </div>
  )
}

// Buttons from Tailwind Plus application-ui/elements/buttons (primary and secondary, on dark).
export const primaryButton =
  'inline-flex justify-center rounded-md bg-brand px-3 py-2 text-sm font-semibold text-on-brand shadow-xs hover:bg-brand-dark focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-brand disabled:opacity-50'

export const secondaryButton =
  'inline-flex justify-center rounded-md bg-ink/10 px-3 py-2 text-sm font-semibold text-ink ring-1 ring-line ring-inset hover:bg-ink/20 disabled:opacity-50'

export function Toggle({
  id,
  label,
  help,
  checked,
  onChange,
}: {
  id: string
  label: string
  help: string
  checked: boolean
  onChange: (checked: boolean) => void
}) {
  return (
    <div className="flex items-center justify-between gap-x-4">
      <span className="flex grow flex-col">
        <label id={`${id}-label`} htmlFor={id} className="text-sm/6 font-medium text-ink">
          {label}
        </label>
        <span id={`${id}-help`} className="text-sm text-muted">
          {help}
        </span>
      </span>
      <div className="group relative inline-flex w-11 shrink-0 rounded-full bg-ink/20 p-0.5 inset-ring inset-ring-ink/5 outline-offset-2 outline-brand transition-colors duration-200 ease-in-out has-checked:bg-brand has-focus-visible:outline-2">
        <span className="size-5 rounded-full bg-surface shadow-xs ring-1 ring-ink/5 transition-transform duration-200 ease-in-out group-has-checked:translate-x-5" />
        <input
          id={id}
          name={id}
          type="checkbox"
          checked={checked}
          onChange={(e) => onChange(e.target.checked)}
          aria-labelledby={`${id}-label`}
          aria-describedby={`${id}-help`}
          className="absolute inset-0 appearance-none focus:outline-hidden"
        />
      </div>
    </div>
  )
}
