// The horizontal Tubarr logo from the brand set, in the version that fits the system theme.
export function Logo({ className }: { className?: string }) {
  return (
    <picture>
      <source srcSet="/tubarr-logo-horizontal-light-notagline.svg" media="(prefers-color-scheme: light)" />
      <img alt="Tubarr" src="/tubarr-logo-horizontal-dark-notagline.svg" className={className} />
    </picture>
  )
}
