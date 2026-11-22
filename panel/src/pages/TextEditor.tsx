import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import {
  BlockNoteSchema,
  createHeadingBlockSpec,
  defaultBlockSpecs,
  defaultInlineContentSpecs,
  defaultStyleSpecs,
  filterSuggestionItems,
} from '@blocknote/core'
import {
  BasicTextStyleButton,
  BlockTypeSelect,
  CreateLinkButton,
  DragHandleMenu,
  FormattingToolbar,
  FormattingToolbarController,
  RemoveBlockItem,
  SideMenu,
  SideMenuController,
  SuggestionMenuController,
  getDefaultReactSlashMenuItems,
  useCreateBlockNote,
} from '@blocknote/react'
import { BlockNoteView } from '@blocknote/mantine'
import '@blocknote/mantine/style.css'
import { currentTheme } from '../theme'
import { blocksToDoc, docToBlocks, type EditorBlock, type PMNode } from './richtext'
import './TextEditor.css'

/** Only what damasco stores: paragraphs, headings 2–4 (the page title is
 * the 1), lists, quotes; bold, italic, code, links. */
const schema = BlockNoteSchema.create({
  blockSpecs: {
    paragraph: defaultBlockSpecs.paragraph,
    heading: createHeadingBlockSpec({ levels: [2, 3, 4], defaultLevel: 2, allowToggleHeadings: false }),
    bulletListItem: defaultBlockSpecs.bulletListItem,
    numberedListItem: defaultBlockSpecs.numberedListItem,
    quote: defaultBlockSpecs.quote,
  },
  inlineContentSpecs: defaultInlineContentSpecs,
  styleSpecs: {
    bold: defaultStyleSpecs.bold,
    italic: defaultStyleSpecs.italic,
    code: defaultStyleSpecs.code,
  },
})

/** The toolbars, defined once: made again at each render of the page, they
 * would close (with the link being typed) whenever the page refreshes. */
function Toolbar() {
  return (
    <FormattingToolbar>
      <BlockTypeSelect key="type" />
      <BasicTextStyleButton key="bold" basicTextStyle="bold" />
      <BasicTextStyleButton key="italic" basicTextStyle="italic" />
      <BasicTextStyleButton key="code" basicTextStyle="code" />
      <CreateLinkButton key="link" />
    </FormattingToolbar>
  )
}

function BlockSideMenu(props: Parameters<typeof SideMenu>[0]) {
  return <SideMenu {...props} dragHandleMenu={BlockMenu} />
}

function BlockMenu(props: Parameters<typeof DragHandleMenu>[0]) {
  return (
    <DragHandleMenu {...props}>
      <RemoveBlockItem {...props}>Delete</RemoveBlockItem>
    </DragHandleMenu>
  )
}

interface TextEditorProps {
  value: PMNode | null | undefined
  onChange: (doc: PMNode) => void
  editable?: boolean
  /** Names the text for screen readers. */
  label: string
}

/** Rich text of a block or a field, edited with BlockNote and kept in the
 * ProseMirror form damasco stores. */
function TextEditor({ value, onChange, editable = true, label }: TextEditorProps) {
  const initial = docToBlocks(value)
  const editor = useCreateBlockNote({
    schema,
    // The editor needs at least one block.
    initialContent: (initial.length ? initial : undefined) as never,
  })
  const changed = useRef(onChange)
  useEffect(() => {
    changed.current = onChange
  })
  // The panel's theme, followed when it changes.
  const [theme, setTheme] = useState(currentTheme())
  useEffect(() => {
    const observer = new MutationObserver(() => setTheme(currentTheme()))
    observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] })
    return () => observer.disconnect()
  }, [])

  return (
    <div className="text-editor" aria-label={label}>
      <BlockNoteView
        editor={editor}
        editable={editable}
        theme={theme}
        formattingToolbar={false}
        sideMenu={false}
        slashMenu={false}
        onChange={() => changed.current(blocksToDoc(editor.document as unknown as EditorBlock[]))}
      >
        <FormattingToolbarController formattingToolbar={Toolbar} />
        <SideMenuController sideMenu={BlockSideMenu} />
        <SuggestionMenuController
          triggerCharacter="/"
          getItems={async (query) => filterSuggestionItems(getDefaultReactSlashMenuItems(editor), query)}
        />
      </BlockNoteView>
    </div>
  )
}

/** The editor reads its value once, then owns it: the page's re-renders
 * (each change, each refresh) leave it alone. */
const SteadyEditor = memo(
  TextEditor,
  (before, after) => before.editable === after.editable && before.label === after.label,
)

/** The change always goes to the handler of the last render: the editor is
 * left alone, and the handler it started with knows the page as it was
 * then, without what changed in the other fields since. */
export default function LatestEditor({ onChange, ...props }: TextEditorProps) {
  const latest = useRef(onChange)
  useLayoutEffect(() => {
    latest.current = onChange
  })
  const change = useCallback((doc: PMNode) => latest.current(doc), [])
  return <SteadyEditor {...props} onChange={change} />
}
