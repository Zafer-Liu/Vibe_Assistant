import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import './index.css'
import './i18n'
import App from './App.tsx'
import { ThemeProvider, getInitialTheme } from './theme.tsx'

// Apply the initial theme (saved choice, else OS preference) before first render to avoid flash
if (getInitialTheme() === 'dark') document.documentElement.classList.add('dark')

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <ThemeProvider>
      <App />
    </ThemeProvider>
  </StrictMode>,
)
