import { memo } from 'react'
import type { ReactNode } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'

/**
 * 帮助文档的正文排版。
 *
 * 与 MemoryMarkdown 分开：记忆条目渲染在卡片里，需要紧凑；帮助说明按文档阅读，
 * 用更宽的行距、真正的表格和独立提示块。
 */
const DOCUMENT_COMPONENTS = {
  h2: ({ children }: { children?: ReactNode }) => (
    <h2 className="mt-7 text-sm font-semibold text-gray-900 first:mt-0 dark:text-gray-100">{children}</h2>
  ),
  h3: ({ children }: { children?: ReactNode }) => (
    <h3 className="mt-6 text-[13px] font-semibold text-gray-900 first:mt-0 dark:text-gray-100">{children}</h3>
  ),
  h4: ({ children }: { children?: ReactNode }) => (
    <h4 className="mt-5 text-[13px] font-semibold text-gray-800 first:mt-0 dark:text-gray-200">{children}</h4>
  ),
  p: ({ children }: { children?: ReactNode }) => (
    <p className="mt-3 text-sm leading-7 text-gray-700 first:mt-0 dark:text-gray-300">{children}</p>
  ),
  ul: ({ children }: { children?: ReactNode }) => (
    <ul className="mt-3 list-disc space-y-1.5 pl-5 text-sm leading-7 text-gray-700 first:mt-0 marker:text-gray-400 dark:text-gray-300 dark:marker:text-gray-500">{children}</ul>
  ),
  ol: ({ children }: { children?: ReactNode }) => (
    <ol className="mt-3 list-decimal space-y-1.5 pl-5 text-sm leading-7 text-gray-700 first:mt-0 marker:text-gray-400 dark:text-gray-300 dark:marker:text-gray-500">{children}</ol>
  ),
  li: ({ children }: { children?: ReactNode }) => <li className="pl-0.5 [&>p]:mt-0">{children}</li>,
  strong: ({ children }: { children?: ReactNode }) => (
    <strong className="font-semibold text-gray-900 dark:text-gray-100">{children}</strong>
  ),
  em: ({ children }: { children?: ReactNode }) => <em className="italic">{children}</em>,
  blockquote: ({ children }: { children?: ReactNode }) => (
    <blockquote className="mt-4 rounded-r-lg border-l-2 border-violet-500 bg-violet-50/70 px-4 py-3 [&>p]:mt-0 [&>p]:text-[13px] [&>p]:leading-6 dark:border-violet-400 dark:bg-violet-500/10">{children}</blockquote>
  ),
  hr: () => <hr className="my-7 border-gray-200 dark:border-gray-800" />,
  a: ({ href, children }: { href?: string; children?: ReactNode }) => (
    <a href={href} target="_blank" rel="noreferrer" className="text-violet-700 underline underline-offset-2 hover:text-violet-800 dark:text-violet-300 dark:hover:text-violet-200">{children}</a>
  ),
  table: ({ children }: { children?: ReactNode }) => (
    <div className="mt-4 overflow-x-auto rounded-lg border border-gray-200 first:mt-0 dark:border-gray-800">
      <table className="w-full border-collapse text-sm">{children}</table>
    </div>
  ),
  thead: ({ children }: { children?: ReactNode }) => (
    <thead className="bg-gray-50 dark:bg-gray-800/60">{children}</thead>
  ),
  tbody: ({ children }: { children?: ReactNode }) => (
    <tbody className="[&>tr:last-child>td]:border-b-0">{children}</tbody>
  ),
  th: ({ children }: { children?: ReactNode }) => (
    <th className="border-b border-gray-200 px-3 py-2 text-left text-xs font-semibold text-gray-700 dark:border-gray-700 dark:text-gray-200">{children}</th>
  ),
  td: ({ children }: { children?: ReactNode }) => (
    <td className="border-b border-gray-100 px-3 py-2 align-top text-[13px] leading-6 text-gray-700 dark:border-gray-800 dark:text-gray-300">{children}</td>
  ),
  code: ({ className, children }: { className?: string; children?: ReactNode }) =>
    className
      ? <code className="font-mono text-xs text-gray-100">{children}</code>
      : <code className="rounded bg-gray-100 px-1 py-0.5 font-mono text-[12px] text-gray-800 dark:bg-gray-800 dark:text-gray-100">{children}</code>,
  pre: ({ children }: { children?: ReactNode }) => (
    <pre className="mt-3 overflow-x-auto rounded-md bg-gray-900 px-4 py-3 text-xs leading-5 text-gray-100 first:mt-0 dark:bg-black">{children}</pre>
  ),
}

export const DocumentMarkdown = memo(function DocumentMarkdown({ content, className = '' }: { content: string; className?: string }) {
  return (
    <div className={`min-w-0 max-w-full break-words [overflow-wrap:anywhere] text-gray-700 dark:text-gray-300 ${className}`}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={DOCUMENT_COMPONENTS}>{content}</ReactMarkdown>
    </div>
  )
})
