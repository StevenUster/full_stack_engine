/**
 * Public types for fse-ssr.
 *
 * `ssr<T>()` hands the template a tree of typed placeholders. To the type
 * checker they behave like the real values (an `SsrValue<string>` is
 * assignable wherever a `string` is expected, can be compared with `===`,
 * used in template literals, …). At build time each placeholder compiles to
 * the matching Tera expression, so the server fills in the real value on
 * every request.
 */
declare const SSR_BRAND: unique symbol;

/**
 * The app's `t.*` shape. Intentionally empty here — the fse-ssr Astro
 * integration generates a `declare module "…/ssr" { interface Translations
 * {...} }` augmentation from the app's own locale JSON (see
 * `.astro/fse-ssr-translations.d.ts` in a consuming project), so `t.*`
 * accesses are typed per app without this package knowing any app's shape.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-interface, @typescript-eslint/no-empty-object-type
export interface Translations {}

/** A server-rendered scalar: types like `T`, renders as `{{ path }}`. */
export type SsrValue<T> = T & { readonly [SSR_BRAND]?: true };

/**
 * A server-rendered list: `.map()` compiles to a Tera `{% for %}` loop whose
 * callback runs exactly once at build time with a placeholder item.
 */
export interface SsrList<E> {
  map<R>(cb: (item: Ssr<E>, index: SsrValue<number>) => R): R;
  /** Compiles to `path | length`. */
  readonly length: SsrValue<number>;
}

/** Maps a plain props interface to its SSR-placeholder counterpart. */
export type Ssr<T> = 0 extends 1 & T // `any` stays `any`
  ? T
  : [T] extends [(infer E)[]]
    ? SsrList<E>
    : [T] extends [string | number | boolean | null | undefined]
      ? SsrValue<T>
      : T extends object
        ? { [K in keyof T]: Ssr<T[K]> }
        : SsrValue<T>;

/**
 * Context every page receives automatically (see the app's
 * `global_context_injector` and the framework's `inject_locale_context`).
 */
export interface GlobalSsrContext {
  t: Translations;
  lang: string;
  /** Every locale keyed by language code, for client-side use. */
  i18n: Record<string, unknown>;
  /** JWT claims when a valid token cookie is present. */
  user?: { sub: string; role: string; [k: string]: unknown };
  can_read_users?: boolean;
}
