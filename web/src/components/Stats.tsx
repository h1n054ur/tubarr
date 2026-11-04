// Ported from Tailwind Plus application-ui/data-display/stats/simple-on-dark.
export interface Stat {
  name: string
  value: string
  unit?: string
}

export function Stats({ stats }: { stats: Stat[] }) {
  return (
    <div className="grid grid-cols-1 gap-px overflow-hidden rounded-lg bg-line sm:grid-cols-2 lg:grid-cols-4">
      {stats.map((stat) => (
        <div key={stat.name} className="bg-surface px-4 py-6 sm:px-6 lg:px-8">
          <p className="text-sm/6 font-medium text-muted">{stat.name}</p>
          <p className="mt-2 flex items-baseline gap-x-2">
            <span className="text-4xl font-semibold tracking-tight text-ink">{stat.value}</span>
            {stat.unit ? <span className="text-sm text-muted">{stat.unit}</span> : null}
          </p>
        </div>
      ))}
    </div>
  )
}
