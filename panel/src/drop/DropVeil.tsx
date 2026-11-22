import { Icon } from '../components/icons'
import './DropVeil.css'

/** Over a place files are dragged onto: what dropping them does, or why
 * nothing can be dropped there now. Its parent is positioned. */
export function DropVeil({
  title,
  hint,
  refused = false,
}: {
  title: string
  hint: string
  refused?: boolean
}) {
  return (
    <div className="drop-veil" data-refused={refused || undefined} aria-hidden="true">
      {!refused && <Icon name="plus" size={28} />}
      <span className="drop-veil-title">{title}</span>
      <span className="drop-veil-hint">{hint}</span>
    </div>
  )
}
