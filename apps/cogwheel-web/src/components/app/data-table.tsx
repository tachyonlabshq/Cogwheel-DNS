import React from "react";
import { ArrowDownIcon, ArrowUpIcon, ChevronsUpDownIcon } from "lucide-react";
import { cn } from "@/lib/utils";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { EmptyState, ErrorState, LoadingSkeleton } from "@/components/app/states";

export type Column<Row> = {
  key: string;
  header: string;
  align?: "start" | "end";
  /** Hidden below `md`, where the table restacks as cards. */
  hideOnStack?: boolean;
  className?: string;
  headClassName?: string;
  render: (row: Row) => React.ReactNode;
  /** Providing this makes the column sortable. */
  sortValue?: (row: Row) => number | string;
};

export type DataTableProps<Row> = {
  columns: Column<Row>[];
  rows: Row[];
  rowKey: (row: Row) => string;
  loading?: boolean;
  error?: string | null;
  onRetry?: () => void;
  empty: { icon: React.ElementType; title: string; description: string; action?: React.ReactNode };
  onRowClick?: (row: Row) => void;
  /** Accessible label describing what the row click does. */
  rowActionLabel?: (row: Row) => string;
  caption?: string;
  className?: string;
};

type SortState = { key: string; direction: "asc" | "desc" } | null;

/**
 * One table implementation for the whole app so loading, empty, error and
 * populated states are impossible to forget. Below `md` the same rows render as
 * stacked label/value cards — the brief forbids horizontal body scroll at
 * 375px, and a six-column table cannot honour that any other way.
 */
export function DataTable<Row>({
  columns,
  rows,
  rowKey,
  loading = false,
  error = null,
  onRetry,
  empty,
  onRowClick,
  rowActionLabel,
  caption,
  className,
}: DataTableProps<Row>) {
  const [sort, setSort] = React.useState<SortState>(null);

  const sorted = React.useMemo(() => {
    if (!sort) return rows;
    const column = columns.find((candidate) => candidate.key === sort.key);
    if (!column?.sortValue) return rows;

    const factor = sort.direction === "asc" ? 1 : -1;
    return [...rows].sort((left, right) => {
      const a = column.sortValue?.(left) ?? "";
      const b = column.sortValue?.(right) ?? "";
      if (typeof a === "number" && typeof b === "number") return (a - b) * factor;
      return String(a).localeCompare(String(b)) * factor;
    });
  }, [columns, rows, sort]);

  const toggleSort = (key: string) => {
    setSort((current) => {
      if (current?.key !== key) return { key, direction: "asc" };
      if (current.direction === "asc") return { key, direction: "desc" };
      return null;
    });
  };

  if (loading && rows.length === 0) return <LoadingSkeleton rows={4} variant="table" />;
  if (error && rows.length === 0) {
    return <ErrorState detail={error} onRetry={onRetry} title="Could not load this list" />;
  }
  if (rows.length === 0) {
    return (
      <EmptyState
        action={empty.action}
        description={empty.description}
        icon={empty.icon}
        title={empty.title}
      />
    );
  }

  const interactive = Boolean(onRowClick);

  return (
    <div className={cn("min-w-0", className)}>
      {error ? (
        <p className="mb-3 text-muted-foreground text-xs">
          Showing last-known rows. Latest refresh failed: {error}
        </p>
      ) : null}

      {/* Table form, md and up. Scrolls inside its own container, never the body. */}
      <div className="hidden overflow-x-auto md:block">
        <Table>
          {caption ? <caption className="sr-only">{caption}</caption> : null}
          <TableHeader>
            <TableRow>
              {columns.map((column) => {
                const isSorted = sort?.key === column.key;
                const SortIcon = !isSorted
                  ? ChevronsUpDownIcon
                  : sort.direction === "asc"
                    ? ArrowUpIcon
                    : ArrowDownIcon;

                return (
                  <TableHead
                    aria-sort={
                      isSorted ? (sort.direction === "asc" ? "ascending" : "descending") : undefined
                    }
                    className={cn(
                      "text-xs",
                      column.align === "end" && "text-right",
                      column.headClassName,
                    )}
                    key={column.key}
                  >
                    {column.sortValue ? (
                      <button
                        className={cn(
                          "-mx-1 inline-flex items-center gap-1 rounded px-1 py-0.5",
                          "hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2",
                          column.align === "end" && "flex-row-reverse",
                        )}
                        onClick={() => toggleSort(column.key)}
                        type="button"
                      >
                        {column.header}
                        <SortIcon aria-hidden className="size-3" />
                      </button>
                    ) : (
                      column.header
                    )}
                  </TableHead>
                );
              })}
            </TableRow>
          </TableHeader>
          <TableBody>
            {sorted.map((row) => (
              <TableRow
                className={cn(interactive && "cursor-pointer")}
                key={rowKey(row)}
                onClick={interactive ? () => onRowClick?.(row) : undefined}
                onKeyDown={
                  interactive
                    ? (event) => {
                        if (event.key === "Enter" || event.key === " ") {
                          event.preventDefault();
                          onRowClick?.(row);
                        }
                      }
                    : undefined
                }
                {...(interactive
                  ? { tabIndex: 0, role: "button", "aria-label": rowActionLabel?.(row) }
                  : {})}
              >
                {columns.map((column) => (
                  <TableCell
                    className={cn(
                      "max-w-[22rem] truncate",
                      column.align === "end" && "text-right",
                      column.className,
                    )}
                    key={column.key}
                  >
                    {column.render(row)}
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>

      {/* Stacked form, below md. */}
      <ul className="flex flex-col gap-2 md:hidden">
        {sorted.map((row) => {
          const body = (
            <dl className="grid gap-1.5">
              {columns
                .filter((column) => !column.hideOnStack)
                .map((column) => (
                  <div className="flex items-start justify-between gap-3" key={column.key}>
                    <dt className="shrink-0 text-muted-foreground text-xs">{column.header}</dt>
                    <dd className="min-w-0 break-words text-right text-foreground text-sm">
                      {column.render(row)}
                    </dd>
                  </div>
                ))}
            </dl>
          );

          return (
            <li key={rowKey(row)}>
              {interactive ? (
                <button
                  aria-label={rowActionLabel?.(row)}
                  className="w-full rounded-xl border border-border p-3 text-left hover:bg-muted/50"
                  onClick={() => onRowClick?.(row)}
                  type="button"
                >
                  {body}
                </button>
              ) : (
                <div className="rounded-xl border border-border p-3">{body}</div>
              )}
            </li>
          );
        })}
      </ul>
    </div>
  );
}
