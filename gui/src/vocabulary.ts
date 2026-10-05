/**
 * What can currently be said, as the client keeps it.
 *
 * A description of the language, not the language. The client guesses at what someone is typing from
 * this; the engine still decides what a command means. Getting a suggestion wrong costs a suggestion —
 * which is why this may be approximate, and why the checking it looks like is not.
 */

export interface CalculationVocabulary { readonly keywords: readonly string[]; readonly operations: readonly string[]; }

export interface Catalogue {
  readonly calculation?: CalculationVocabulary;
  readonly commands: readonly MetaCommand[];
  readonly annotations: readonly string[];
  readonly providers: readonly Provider[];
  readonly templates?: readonly Template[];
  /** Every type name the workspace currently resolves: built-ins, loaded records, and List/Map/Option/Iter. */
  readonly types?: readonly string[];
}

export interface Template {
  readonly name: string;
  readonly body: string;
  readonly parameters: readonly Parameter[];
}

/** A subcommand: the word, its complete parameter set, and the words that may follow it. */
export interface CommandVariant {
  readonly word: string;
  readonly parameters: readonly Parameter[];
  readonly takes: readonly string[];
  readonly variants: readonly CommandVariant[];
}

export interface MetaCommand {
  readonly name: string;
  readonly implemented: boolean;
  readonly summary: string;
  /**
   * The words that may follow, when only certain ones may — ':list' takes exactly seven. Empty when the
   * command takes any word, which is not the same as taking none.
   */
  readonly takes: readonly string[];
  /** Each advertised subcommand with its own parameters and, recursively, the words after it. */
  readonly variants: readonly CommandVariant[];
  readonly open: boolean;
  readonly parameters: readonly Parameter[];
}

export interface Provider {
  readonly name: string;
  readonly capabilities: readonly Capability[];
  /**
   * The credentials it needs, by name, and whether each has been given.
   *
   * A method can require several credentials, such as a key and a secret in separate headers.
   * Track each one's availability so the UI can identify exactly which is missing.
   * Credential values never appear here.
   */
  readonly credentials: readonly Credential[];
  /** Whether all of them have been supplied. */
  readonly ready: boolean;
}

export interface Credential {
  readonly name: string;
  readonly supplied: boolean;
}

/** The providers that named a credential, whether or not one has been given. */
export function needingCredentials(catalogue: Catalogue): readonly Provider[] {
  return catalogue.providers.filter((provider) => provider.credentials.length > 0);
}

export interface Capability {
  readonly path: readonly string[];
  readonly summary: string;
  readonly result: string;
  readonly safe: boolean;
  readonly parameters: readonly Parameter[];
}

export interface Parameter {
  readonly resourceHint?: string;
  readonly resources?: { readonly node: string; readonly observedAtNs: string; readonly items: readonly { readonly value: string; readonly label: string; readonly detail: string }[] };
  readonly name: string;
  readonly type: string;
  readonly required: boolean;
  /** The values a rule allows. Empty when nothing constrains it. */
  readonly allowed: readonly string[];
  /**
   * What language the value is written in, or empty when it is only a value.
   *
   * <p>`sh run cmd:"…"` carries a shell line: `Text` at the type level and a whole other language
   * inside it. Nothing here can complete that — the names are files and programs on the machine the
   * engine runs on — so this is the flag that says to go and ask.
   */
  readonly content: string;
}

export const emptyCatalogue: Catalogue = { commands: [], annotations: [], providers: [] };

export function providerNamed(catalogue: Catalogue, name: string): Provider | undefined {
  return catalogue.providers.find((provider) => provider.name === name);
}

export function commandNamed(catalogue: Catalogue, name: string): MetaCommand | undefined {
  return catalogue.commands.find((command) => command.name === name);
}

/**
 * The capability whose path is the longest prefix of what has been typed.
 *
 * Longest match rather than exact, because someone halfway through `kubectl get pods` has typed a path
 * that matches nothing yet, and the useful answer is what they are heading towards.
 */
export function capabilityUnder(
  provider: Provider,
  words: readonly string[],
): Capability | undefined {
  return provider.capabilities.find(
    (capability) =>
      capability.path.length <= words.length &&
      capability.path.every((segment, index) => words[index] === segment),
  );
}
