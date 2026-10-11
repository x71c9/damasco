import type { ButtonHTMLAttributes } from 'react'
import { buttonClass, type ButtonVariant } from './buttonClass'
import { Icon, type IconName } from './icons'

interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant
  /** 36 px tall instead of 44, for crowded places. */
  small?: boolean
  /** An icon before the label. */
  icon?: IconName
}

export function Button({
  variant = 'secondary',
  small = false,
  icon,
  className,
  type = 'button',
  children,
  ...rest
}: ButtonProps) {
  return (
    <button
      type={type}
      className={[buttonClass(variant, small), className].filter(Boolean).join(' ')}
      {...rest}
    >
      {icon && <Icon name={icon} size={small ? 15 : 17} strokeWidth={1.75} />}
      {children}
    </button>
  )
}
