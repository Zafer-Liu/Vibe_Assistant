import { memo, useMemo } from 'react'
import { useTranslation } from 'react-i18next'
import { HelpDocument } from '../components/HelpDocument'

/** 记忆功能的机制性说明统一放在这里；正常页面只保留一句话提示与入口。 */
const HELP_TOPICS = ['layers', 'capture', 'injection', 'noHook', 'memoryMcpObservation', 'dataDir'] as const

export const MemoryHelp = memo(function MemoryHelp({ onBack }: { onBack: () => void }) {
  const { t } = useTranslation()
  const sections = useMemo(() => HELP_TOPICS.map(topic => ({
    id: topic,
    title: t(`memory.help.${topic}.title`),
    body: t(`memory.help.${topic}.body`),
  })), [t])
  return (
    <HelpDocument
      domain={t('nav.memory')}
      title={t('nav.memoryHelp')}
      summary={t('memory.helpSubtitle')}
      sections={sections}
      onBack={onBack}
      backLabel={t('memory.helpBack')}
    />
  )
})
