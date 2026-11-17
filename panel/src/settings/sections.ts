/** The sections inside settings, in the order of their menu. */
export const settingsSections = ['people', 'activity'] as const
export type SettingsSection = (typeof settingsSections)[number]
