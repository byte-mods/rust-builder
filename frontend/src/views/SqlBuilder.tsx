import { useState, useEffect, useMemo } from "react";
import { fetchDbSchema, type DbSchemaReport, type DbTable } from "../api";

interface SqlBuilderProps {
  slug: string;
  connectionString: string;
  initialQuery: string;
  onSave: (query: string) => void;
  onClose: () => void;
}

interface FilterCriteria {
  column: string;
  operator: string;
  value: string;
}

export default function SqlBuilder({
  slug,
  connectionString,
  initialQuery: _initialQuery,
  onSave,
  onClose,
}: SqlBuilderProps): JSX.Element {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [schema, setSchema] = useState<DbSchemaReport | null>(null);

  // Builder States
  const [selectedTable, setSelectedTable] = useState("");
  const [selectedColumns, setSelectedColumns] = useState<string[]>([]);
  const [filters, setFilters] = useState<FilterCriteria[]>([]);
  const [orderByColumn, setOrderByColumn] = useState("");
  const [orderByDirection, setOrderByDirection] = useState<"ASC" | "DESC">("ASC");
  const [limit, setLimit] = useState("");
  const [offset, setOffset] = useState("");

  // Connect and explore database schema on mount or connectionString change
  useEffect(() => {
    if (!connectionString) {
      setError("Please configure a database Connection String in the node properties first.");
      return;
    }

    let active = true;
    async function loadSchema() {
      setLoading(true);
      setError(null);
      try {
        const res = await fetchDbSchema(slug, connectionString);
        if (active) {
          setSchema(res);
          if (res.tables.length > 0) {
            setSelectedTable(res.tables[0].name);
          }
        }
      } catch (err: any) {
        if (active) {
          setError(err.message || "Failed to connect to database. Please verify your connection string and TCP ports.");
        }
      } finally {
        if (active) {
          setLoading(false);
        }
      }
    }

    loadSchema();
    return () => {
      active = false;
    };
  }, [slug, connectionString]);

  // Retrieve current active table details
  const activeTable = useMemo<DbTable | undefined>(() => {
    return schema?.tables.find((t) => t.name === selectedTable);
  }, [schema, selectedTable]);

  // Reset columns and filters whenever table changes
  useEffect(() => {
    setSelectedColumns([]);
    setFilters([]);
    setOrderByColumn("");
    setLimit("");
    setOffset("");
  }, [selectedTable]);

  // Construct dynamic SQL statement from visual properties in real-time
  const generatedQuery = useMemo(() => {
    if (!selectedTable) return "";

    let cols = selectedColumns.length > 0 ? selectedColumns.join(", ") : "*";
    let query = `SELECT ${cols} FROM ${selectedTable}`;

    if (filters.length > 0) {
      const isPostgres = connectionString.startsWith("postgres:") || connectionString.startsWith("postgresql:");
      const filterStrings = filters.map((f, i) => {
        const placeholder = isPostgres ? `$${i + 1}` : "?";
        if (f.operator === "IS NULL" || f.operator === "IS NOT NULL") {
          return `${f.column} ${f.operator}`;
        }
        let val = f.value.trim();
        if (val.length === 0) {
          val = placeholder;
        }
        return `${f.column} ${f.operator} ${val}`;
      });
      query += ` WHERE ${filterStrings.join(" AND ")}`;
    }

    if (orderByColumn) {
      query += ` ORDER BY ${orderByColumn} ${orderByDirection}`;
    }

    if (limit.trim().length > 0) {
      query += ` LIMIT ${limit.trim()}`;
    }

    if (offset.trim().length > 0) {
      query += ` OFFSET ${offset.trim()}`;
    }

    return query + ";";
  }, [selectedTable, selectedColumns, filters, orderByColumn, orderByDirection, limit, offset, connectionString]);

  // Handle column checkbox select toggle
  function toggleColumn(colName: string) {
    setSelectedColumns((prev) =>
      prev.includes(colName) ? prev.filter((c) => c !== colName) : [...prev, colName]
    );
  }

  // Handle select all projection columns toggle
  function toggleSelectAll() {
    if (!activeTable) return;
    const allCols = activeTable.columns.map((c) => c.name);
    if (selectedColumns.length === allCols.length) {
      setSelectedColumns([]);
    } else {
      setSelectedColumns(allCols);
    }
  }

  // Add filter row criteria
  function addFilter() {
    if (!activeTable || activeTable.columns.length === 0) return;
    setFilters((prev) => [
      ...prev,
      { column: activeTable.columns[0].name, operator: "=", value: "" },
    ]);
  }

  // Remove filter row criteria
  function removeFilter(index: number) {
    setFilters((prev) => prev.filter((_, i) => i !== index));
  }

  // Update specific filter criteria fields
  function updateFilter(index: number, key: keyof FilterCriteria, val: string) {
    setFilters((prev) => {
      const copy = [...prev];
      copy[index] = { ...copy[index], [key]: val };
      return copy;
    });
  }

  return (
    <div className="sql-builder-container">
      {/* Title Header */}
      <div className="sql-builder-header">
        <div className="title-area">
          <span className="sparkle">🪄</span>
          <h3>Visual SQL Builder</h3>
        </div>
        <div className="action-buttons">
          <button type="button" className="btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn-primary"
            disabled={!selectedTable}
            onClick={() => onSave(generatedQuery)}
          >
            Update Node Query
          </button>
        </div>
      </div>

      {/* Loading Overlay */}
      {loading && (
        <div className="sql-builder-status-card loading-state">
          <div className="spinner"></div>
          <p>Connecting and exploring database schema over TCP...</p>
        </div>
      )}

      {/* Error Card */}
      {error && !loading && (
        <div className="sql-builder-status-card error-state">
          <span className="warn-icon">⚠️</span>
          <h4>Connection Failed</h4>
          <p>{error}</p>
          <button type="button" className="btn-retry" onClick={() => window.location.reload()}>
            Retry
          </button>
        </div>
      )}

      {/* Main Designer Studio */}
      {schema && !loading && !error && (
        <div className="sql-builder-workspace">
          {/* Table Selector bar */}
          <div className="table-selector-section">
            <label>
              Select Database Table
              <select value={selectedTable} onChange={(e) => setSelectedTable(e.target.value)}>
                {schema.tables.map((t) => (
                  <option key={t.name} value={t.name}>
                    {t.name} ({t.columns.length} columns)
                  </option>
                ))}
              </select>
            </label>
          </div>

          <div className="builder-split-panel">
            {/* Columns Projector */}
            <div className="builder-panel columns-panel">
              <div className="panel-header">
                <h4>1. Project Columns (SELECT)</h4>
                {activeTable && (
                  <button type="button" className="text-btn" onClick={toggleSelectAll}>
                    {selectedColumns.length === activeTable.columns.length ? "Clear All" : "Select All"}
                  </button>
                )}
              </div>
              <div className="columns-list">
                {activeTable?.columns.map((col) => {
                  const isChecked = selectedColumns.includes(col.name);
                  return (
                    <label key={col.name} className={`column-row-item ${isChecked ? "active" : ""}`}>
                      <input
                        type="checkbox"
                        checked={isChecked}
                        onChange={() => toggleColumn(col.name)}
                      />
                      <span className="col-name">{col.name}</span>
                      <span className="col-type">{col.data_type}</span>
                      {col.primary_key && <span className="badge-pk">PK</span>}
                      {activeTable.relations.some((r) => r.column === col.name) && (
                        <span className="badge-fk">FK</span>
                      )}
                    </label>
                  );
                })}
              </div>
            </div>

            {/* Criteria Editor */}
            <div className="builder-panel criteria-panel">
              <h4>2. Filter & Sort Criteria</h4>
              
              {/* WHERE clauses */}
              <div className="criteria-section">
                <div className="section-header">
                  <h5>Filters (WHERE)</h5>
                  <button type="button" className="btn-add-criteria" onClick={addFilter}>
                    + Add Filter
                  </button>
                </div>
                {filters.length === 0 ? (
                  <p className="muted-hint">No filter criteria added. Query will fetch all table rows.</p>
                ) : (
                  <div className="filters-list">
                    {filters.map((filter, i) => (
                      <div key={i} className="filter-row">
                        <select
                          value={filter.column}
                          onChange={(e) => updateFilter(i, "column", e.target.value)}
                        >
                          {activeTable?.columns.map((c) => (
                            <option key={c.name} value={c.name}>
                              {c.name}
                            </option>
                          ))}
                        </select>
                        <select
                          value={filter.operator}
                          onChange={(e) => updateFilter(i, "operator", e.target.value)}
                        >
                          <option value="=">=</option>
                          <option value=">">&gt;</option>
                          <option value="<">&lt;</option>
                          <option value=">=">&gt;=</option>
                          <option value="<=">&lt;=</option>
                          <option value="LIKE">LIKE</option>
                          <option value="IS NULL">IS NULL</option>
                          <option value="IS NOT NULL">IS NOT NULL</option>
                        </select>
                        {filter.operator !== "IS NULL" && filter.operator !== "IS NOT NULL" && (
                          <input
                            type="text"
                            placeholder="Value or placeholder (e.g. $1)"
                            value={filter.value}
                            onChange={(e) => updateFilter(i, "value", e.target.value)}
                          />
                        )}
                        <button type="button" className="btn-remove" onClick={() => removeFilter(i)}>
                          ✕
                        </button>
                      </div>
                    ))}
                  </div>
                )}
              </div>

              {/* ORDER BY */}
              <div className="criteria-section">
                <h5>Sort Order (ORDER BY)</h5>
                <div className="orderby-row">
                  <select value={orderByColumn} onChange={(e) => setOrderByColumn(e.target.value)}>
                    <option value="">-- No Sorting --</option>
                    {activeTable?.columns.map((c) => (
                      <option key={c.name} value={c.name}>
                        {c.name}
                      </option>
                    ))}
                  </select>
                  {orderByColumn && (
                    <select
                      value={orderByDirection}
                      onChange={(e) => setOrderByDirection(e.target.value as "ASC" | "DESC")}
                    >
                      <option value="ASC">ASC (Ascending)</option>
                      <option value="DESC">DESC (Descending)</option>
                    </select>
                  )}
                </div>
              </div>

              {/* LIMIT / OFFSET */}
              <div className="criteria-section limit-offset-section">
                <label>
                  Limit
                  <input
                    type="number"
                    min="0"
                    placeholder="None"
                    value={limit}
                    onChange={(e) => setLimit(e.target.value)}
                  />
                </label>
                <label>
                  Offset
                  <input
                    type="number"
                    min="0"
                    placeholder="None"
                    value={offset}
                    onChange={(e) => setOffset(e.target.value)}
                  />
                </label>
              </div>
            </div>
          </div>

          {/* Generated SQL Code Box */}
          <div className="sql-preview-section">
            <h5>3. Live SQL Query Preview</h5>
            <pre className="query-code-preview">
              <code>{generatedQuery}</code>
            </pre>
          </div>
        </div>
      )}
    </div>
  );
}
