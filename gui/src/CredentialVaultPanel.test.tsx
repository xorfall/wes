import { afterEach, expect, it, vi } from "vitest";
import { act, create, type ReactTestRenderer } from "react-test-renderer";
import { CredentialVaultPanel } from "./CredentialVaultPanel";
import { applicationLog } from "./application-log";

let tree: ReactTestRenderer | undefined;
afterEach(() => { if (tree) act(() => tree!.unmount()); tree = undefined; vi.unstubAllGlobals(); vi.restoreAllMocks(); });

type Reply = { ok: boolean; body: unknown };
/** A synthetic host: GET answers the current state, each POST takes the next reply. */
function host(state: unknown, replies: Reply[] = []) {
  const posts: unknown[] = [];
  const fetch = vi.fn(async (_url: string, init: RequestInit) => {
    if (init.method === "GET") return { ok: true, json: async () => state };
    posts.push(JSON.parse(init.body as string));
    const reply = replies.shift()!;
    if (reply.ok) state = reply.body;
    return { ok: reply.ok, json: async () => reply.body, text: async () => String(reply.body) };
  });
  vi.stubGlobal("fetch", fetch);
  return { fetch, posts };
}
const text = () => JSON.stringify(tree!.toJSON());
const button = (label: string) => tree!.root.findAllByType("button").find(b => b.children.join("") === label);
const input = (label: string) => tree!.root.findByProps({ "aria-label": label });
const type = (label: string, value: string) => act(() => input(label).props.onChange({ target: { value } }));
const submit = () => act(async () => { tree!.root.findByType("form").props.onSubmit({ preventDefault() {} }); });

it("should_describe_the_system_store_without_vault_controls_when_the_platform_has_one", async () => {
  // Arrange
  host({ kind: "system" });

  // Act
  await act(async () => { tree = create(<CredentialVaultPanel />); });

  // Assert
  expect(text()).toContain("system secure store");
  expect(tree!.root.findAllByType("input")).toHaveLength(0);
  expect(tree!.root.findAllByType("button")).toHaveLength(0);
});

it("should_create_the_vault_only_with_a_long_repeated_password_and_clear_the_fields", async () => {
  // Arrange
  const { posts } = host({ kind: "vault", state: "absent" }, [{ ok: true, body: { kind: "vault", state: "unlocked" } }]);
  await act(async () => { tree = create(<CredentialVaultPanel />); });

  // Act
  type("Vault password", "short");
  const shortDisabled = button("Create vault")!.props.disabled;
  type("Vault password", "synthetic password");
  type("Repeat vault password", "different password");
  const mismatchDisabled = button("Create vault")!.props.disabled;
  type("Repeat vault password", "synthetic password");
  await submit();

  // Assert
  expect(shortDisabled).toBe(true);
  expect(mismatchDisabled).toBe(true);
  expect(posts).toEqual([{ action: "create", password: "synthetic password" }]);
  expect(text()).toContain("unlocked");
  expect(text()).toContain("Credential vault created and unlocked.");
  expect(text()).not.toContain("synthetic password");
  expect(button("Lock")).toBeDefined();
});

it("should_stay_locked_and_show_the_host_reason_without_the_password_after_a_wrong_unlock", async () => {
  // Arrange
  const log = vi.spyOn(applicationLog, "add");
  const { posts } = host({ kind: "vault", state: "locked" }, [{ ok: false, body: "The password is incorrect or the vault file was changed" }]);
  await act(async () => { tree = create(<CredentialVaultPanel />); });

  // Act
  type("Vault password", "synthetic wrong password");
  await submit();

  // Assert
  expect(posts).toEqual([{ action: "unlock", password: "synthetic wrong password" }]);
  expect(text()).toContain("locked");
  expect(tree!.root.findByProps({ role: "status" }).children.join("")).toContain("password is incorrect");
  expect(input("Vault password").props.value).toBe("");
  expect(text()).not.toContain("synthetic wrong password");
  expect(JSON.stringify(log.mock.calls)).not.toContain("synthetic wrong password");
});

it("should_ask_again_before_a_reset_and_send_its_explicit_confirmation", async () => {
  // Arrange
  const { posts } = host({ kind: "vault", state: "unlocked" }, [{ ok: true, body: { kind: "vault", state: "absent" } }]);
  await act(async () => { tree = create(<CredentialVaultPanel />); });

  // Act
  act(() => button("Reset vault")!.props.onClick());
  const asked = text().includes("Remove every remembered credential?");
  act(() => button("Cancel")!.props.onClick());
  const cancelledPosts = posts.length;
  act(() => button("Reset vault")!.props.onClick());
  await act(async () => { button("Confirm reset")!.props.onClick(); });

  // Assert
  expect(asked).toBe(true);
  expect(cancelledPosts).toBe(0);
  expect(posts).toEqual([{ action: "reset", confirm: "reset" }]);
  expect(text()).toContain("not created");
  expect(text()).toContain("Workspaces and data are unchanged.");
});
