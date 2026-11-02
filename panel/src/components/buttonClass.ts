/** The kinds of buttons of the design system:
 * - primary: the main action of a place, one per screen or dialog;
 * - secondary: the other actions;
 * - quiet: minor actions that should not draw the eye;
 * - danger: actions that remove or stop something; never the only way out;
 * - accent: the colour of Publish, for what the assistant hands over to the
 *   person, such as its buttons into the panel. */
export type ButtonVariant = 'primary' | 'secondary' | 'quiet' | 'danger' | 'accent'

/** The class names of a button, for elements that look like one (a link). */
export function buttonClass(variant: ButtonVariant = 'secondary', small = false): string {
  return ['button', `button-${variant}`, small ? 'button-small' : ''].filter(Boolean).join(' ')
}
