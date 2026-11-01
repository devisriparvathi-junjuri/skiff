/**
 * UI words. `en.ts` is the source catalog: every key lives there, in English.
 * Another language is a `Partial<Messages>`; a key it lacks falls back to
 * English. `t("cmd.newSession")`, `t("palette.openProject", { name })`.
 */
import { en } from "./en";
import { es } from "./es";
import { zh } from "./zh";

export type Messages = typeof en;
export type MessageKey = keyof Messages;
export type Vars = Record<string, string | number>;

const catalogs: Record<string, Partial<Record<MessageKey, string>>> = { en, es, zh };

let current: Partial<Record<MessageKey, string>> = en;
let currentLang = "en";
let plurals = new Intl.PluralRules("en");

/** The languages Skiff has words for. */
export const languages = () => Object.keys(catalogs);

/** A language's name in that language: "English", "Español". */
export const languageName = (code: string) => catalogs[code]?.["language.name"] ?? en["language.name"];

/**
 * The catalog a language code picks: "es-MX" gives "es"; one with no catalog
 * gives "en". `zh` is Simplified Chinese, so Traditional (zh-TW, zh-HK,
 * zh-MO, zh-Hant) gets English until it has a catalog of its own.
 */
export const catalogFor = (lang: string) => {
  const [base, ...rest] = lang.toLowerCase().split(/[-_]/);
  if (base === "zh" && !rest.includes("hans") && rest.some((part) => ["hant", "tw", "hk", "mo"].includes(part))) return "en";
  return catalogs[base] ? base : "en";
};

/** The catalog the system language picks: what Auto gives. */
export const systemLanguage = () => catalogFor(navigator.language);

/** Picks the catalog for "de" or "de-AT"; an unknown language gives English. */
export function setLanguage(lang: string) {
  currentLang = catalogFor(lang);
  current = catalogs[currentLang];
  plurals = new Intl.PluralRules(currentLang);
  document.documentElement.lang = currentLang;
}

export const language = () => currentLang;

/**
 * The locale for dates, times and numbers: the system's own ("en-GB", "es-MX")
 * when it speaks the current language, else the language itself.
 */
export const locale = () => (catalogFor(navigator.language) === currentLang ? navigator.language : currentLang);

/** The words for `key` in the current language, with each `{name}` filled from `vars`. */
export function t(key: MessageKey, vars?: Vars): string {
  const text = current[key] ?? en[key];
  return vars ? text.replace(/\{([\w-]+)\}/g, (all, name: string) => (name in vars ? String(vars[name]) : all)) : text;
}

/** A key with plural forms: `x#one`, `x#other` and any form the language adds (`x#few`, `x#many`). */
export type PluralKey = { [K in MessageKey]: K extends `${infer B}#other` ? B : never }[MessageKey];

/** The form of `key` for `count`, by the language's plural rules. `{count}` is filled in. */
export function tn(key: PluralKey, count: number, vars?: Vars): string {
  const form = `${key}#${plurals.select(count)}` as MessageKey;
  return t(form in current || form in en ? form : (`${key}#other` as MessageKey), { count, ...vars });
}

/** Whole sentences in one line: a space between them, or none where the language puts none (Chinese). */
export const sentences = (parts: string[]) => parts.join(t("sentence.join"));

/** Capitals by the language's rules, for menu and sheet heads. */
export const upper = (text: string) => text.toLocaleUpperCase(currentLang);
