// Package-tree sidebar (Section 1 T5).
//
// Renders the project's package tree, lets the user select a package,
// create children, rename inline, and delete. Selection drives the
// canvas: the parent component swaps the graph it edits to the
// selected package's graph.
//
// State ownership:
//   - Tree data + selection live in the parent (`ProjectCanvas`) so a
//     graph save knows which package to write to. This component is
//     purely view + event-emitter.
//   - Expansion state (which subtrees are open) is local to this
//     component; it's a UI preference, not persisted.

import { useMemo, useState } from "react";
import type { Package } from "../api";

interface PackageTreeProps {
  packages: Package[];
  /** Currently-selected package slug (drives canvas content). */
  selectedSlug: string;
  /** Called when the user clicks a package row. */
  onSelect: (pkgSlug: string) => void;
  /** Called with `(parentId | null, slug, label?)` when the user
   *  creates a child. `parentId` of `null` means "default to root". */
  onCreate: (parentId: string | null, slug: string) => Promise<void>;
  /** Called when the user finishes an inline rename. New slug is
   *  validated server-side; UI surfaces 409/422 via the error prop. */
  onRename: (pkgSlug: string, newSlug: string) => Promise<void>;
  /** Called when the user confirms a delete. Root is not deletable. */
  onDelete: (pkgSlug: string) => Promise<void>;
  /** Optional error message surfaced inline from the last operation. */
  errorMessage?: string | null;
}

/**
 * Group packages by `parent_id` once so child lookups during recursive
 * render are O(1). Re-runs only when the `packages` array reference
 * changes.
 */
function useChildrenMap(packages: Package[]): Map<string | null, Package[]> {
  return useMemo(() => {
    const m = new Map<string | null, Package[]>();
    for (const p of packages) {
      const key = p.parent_id ?? null;
      const arr = m.get(key) ?? [];
      arr.push(p);
      m.set(key, arr);
    }
    // Stable, alphabetical sibling order — matches the backend codegen.
    for (const arr of m.values()) {
      arr.sort((a, b) => a.slug.localeCompare(b.slug));
    }
    return m;
  }, [packages]);
}

export function PackageTree({
  packages,
  selectedSlug,
  onSelect,
  onCreate,
  onRename,
  onDelete,
  errorMessage,
}: PackageTreeProps) {
  const childrenMap = useChildrenMap(packages);
  const root = packages.find((p) => p.parent_id === null);

  // Track which subtrees are expanded. Root is always expanded — the
  // user can't collapse the only entry point.
  const [expanded, setExpanded] = useState<Set<string>>(
    () => new Set(root ? [root.id] : []),
  );
  const toggle = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  // Inline-create / inline-rename state is held here so only one row
  // at a time is editable. `createUnderParent` is the parent id (or
  // `null` for root) the user is currently adding a child to.
  const [createUnderParent, setCreateUnderParent] = useState<
    string | null | "off"
  >("off");
  const [createSlugDraft, setCreateSlugDraft] = useState("");
  const [renameTargetSlug, setRenameTargetSlug] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");

  if (!root) {
    return (
      <aside className="package-tree package-tree-empty">
        <div className="package-tree-header">Packages</div>
        <div className="package-tree-empty-msg">No packages yet.</div>
      </aside>
    );
  }

  /**
   * Render one package row + recursively its children if expanded.
   * Indentation comes from CSS via `data-depth`, so the rendered DOM
   * is flat-per-row (better for screen readers than nested ULs).
   */
  function renderRow(pkg: Package, depth: number): React.ReactNode {
    const children = childrenMap.get(pkg.id) ?? [];
    const isRoot = pkg.parent_id === null;
    const isSelected = pkg.slug === selectedSlug;
    const isExpanded = expanded.has(pkg.id);
    const isRenaming = renameTargetSlug === pkg.slug;

    return (
      <div
        key={pkg.id}
        className="package-tree-subtree"
        data-depth={depth}
      >
        <div
          className={
            "package-tree-row" +
            (isSelected ? " package-tree-row-selected" : "")
          }
          style={{ paddingLeft: `${8 + depth * 14}px` }}
        >
          {children.length > 0 ? (
            <button
              type="button"
              className="package-tree-expander"
              onClick={() => toggle(pkg.id)}
              aria-label={isExpanded ? "Collapse" : "Expand"}
            >
              {isExpanded ? "▾" : "▸"}
            </button>
          ) : (
            <span className="package-tree-expander-spacer" />
          )}

          {isRenaming ? (
            <input
              type="text"
              className="package-tree-rename-input"
              autoFocus
              value={renameDraft}
              onChange={(e) => setRenameDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  void onRename(pkg.slug, renameDraft).then(() => {
                    setRenameTargetSlug(null);
                  });
                } else if (e.key === "Escape") {
                  setRenameTargetSlug(null);
                }
              }}
              onBlur={() => setRenameTargetSlug(null)}
            />
          ) : (
            <button
              type="button"
              className="package-tree-label"
              onClick={() => onSelect(pkg.slug)}
              onDoubleClick={() => {
                if (!isRoot) {
                  setRenameDraft(pkg.slug);
                  setRenameTargetSlug(pkg.slug);
                }
              }}
              title={
                isRoot
                  ? "Root package (cannot rename or delete)"
                  : "Click to open · double-click to rename"
              }
            >
              {pkg.label ?? pkg.slug}
              {isRoot ? <span className="package-tree-root-badge">root</span> : null}
            </button>
          )}

          <div className="package-tree-row-actions">
            <button
              type="button"
              className="package-tree-action"
              onClick={() => {
                setCreateUnderParent(pkg.id);
                setCreateSlugDraft("");
              }}
              title="Add child package"
              aria-label="Add child package"
            >
              +
            </button>
            {!isRoot ? (
              <button
                type="button"
                className="package-tree-action package-tree-action-delete"
                onClick={() => {
                  if (
                    window.confirm(
                      `Delete package "${pkg.slug}" and every descendant?`,
                    )
                  ) {
                    void onDelete(pkg.slug);
                  }
                }}
                title="Delete package"
                aria-label="Delete package"
              >
                ×
              </button>
            ) : null}
          </div>
        </div>

        {createUnderParent === pkg.id ? (
          <div
            className="package-tree-create-row"
            style={{ paddingLeft: `${8 + (depth + 1) * 14}px` }}
          >
            <input
              type="text"
              autoFocus
              placeholder="new-package-slug"
              value={createSlugDraft}
              onChange={(e) => setCreateSlugDraft(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && createSlugDraft.trim().length > 0) {
                  const slug = createSlugDraft.trim();
                  void onCreate(pkg.id, slug).then(() => {
                    setCreateUnderParent("off");
                    setCreateSlugDraft("");
                  });
                } else if (e.key === "Escape") {
                  setCreateUnderParent("off");
                  setCreateSlugDraft("");
                }
              }}
              onBlur={() => {
                // Cancel-on-blur if no input typed — saves a click.
                if (createSlugDraft.trim().length === 0) {
                  setCreateUnderParent("off");
                }
              }}
            />
          </div>
        ) : null}

        {isExpanded
          ? children.map((c) => renderRow(c, depth + 1))
          : null}
      </div>
    );
  }

  return (
    <aside className="package-tree">
      <div className="package-tree-header">Packages</div>
      {errorMessage ? (
        <div className="package-tree-error" role="alert">
          {errorMessage}
        </div>
      ) : null}
      <div className="package-tree-body">{renderRow(root, 0)}</div>
    </aside>
  );
}
