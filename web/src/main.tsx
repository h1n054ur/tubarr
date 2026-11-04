import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { QueryClientProvider } from '@tanstack/react-query'
import { createBrowserHistory } from '@tanstack/react-router'
import { App } from './App'
import { createQueryClient } from './lib/auth'
// Lexend served from the UI itself, only the weights the components use (font-normal, -medium,
// -semibold, -bold), so the browser asks no outside host for anything.
import '@fontsource/lexend/400.css'
import '@fontsource/lexend/500.css'
import '@fontsource/lexend/600.css'
import '@fontsource/lexend/700.css'
import './index.css'

const queryClient = createQueryClient()
const history = createBrowserHistory()

const root = document.getElementById('root')
if (root) {
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queryClient}>
        <App history={history} />
      </QueryClientProvider>
    </StrictMode>,
  )
}
