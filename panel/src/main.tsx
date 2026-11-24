import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import '@fontsource-variable/geist'
import '@fontsource-variable/geist-mono'
import './styles/tokens.css'
import './styles/base.css'
import { App } from './App'
import { applyStoredTheme } from './theme'
import { watchFileDrags } from './drop/files'
import { watchSheets, watchViewport } from './viewport'

applyStoredTheme()
watchFileDrags()
watchViewport()
watchSheets()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
