/** The sections inside settings, in the order of their menu. */
export const settingsSections = ['assistant', 'instructions', 'sites', 'builds'] as const
export type SettingsSection = (typeof settingsSections)[number]

export function isSettings(section: string): section is SettingsSection {
  return (settingsSections as readonly string[]).includes(section)
}
