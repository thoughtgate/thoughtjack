import React from 'react';

type CellStatus = 'covered' | 'gap' | 'out_of_scope';

interface CoverageCell {
  row: string;
  column: string;
  status: CellStatus;
  scenarios?: string[];
}

interface ScopeExclusion {
  row: string;
  column: string;
  reason: string;
}

interface CoverageMatrixProps {
  rows: string[];
  columns: string[];
  data: CoverageCell[];
  scopeExclusions?: ScopeExclusion[];
}

function cellClass(status: CellStatus): string {
  return `tj-coverage-cell tj-coverage-cell--${status}`;
}

function cellTitle(cell: CoverageCell | undefined, exclusion: ScopeExclusion | undefined): string {
  if (exclusion) return `Out of scope: ${exclusion.reason}`;
  if (!cell) return 'No data';
  if (cell.scenarios && cell.scenarios.length > 0) {
    return cell.scenarios.join(', ');
  }
  return cell.status;
}

export function CoverageMatrix({
  rows,
  columns,
  data,
  scopeExclusions = [],
}: CoverageMatrixProps): React.ReactElement {
  const lookup = new Map<string, CoverageCell>();
  for (const cell of data) {
    lookup.set(`${cell.row}:${cell.column}`, cell);
  }

  const exclusionLookup = new Map<string, ScopeExclusion>();
  for (const ex of scopeExclusions) {
    exclusionLookup.set(`${ex.row}:${ex.column}`, ex);
  }

  return (
    <div className="tj-coverage-matrix">
      <table>
        <thead>
          <tr>
            <th></th>
            {columns.map((col) => (
              <th key={col}>{col}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr key={row}>
              <td><strong>{row}</strong></td>
              {columns.map((col) => {
                const key = `${row}:${col}`;
                const cell = lookup.get(key);
                const exclusion = exclusionLookup.get(key);
                const status: CellStatus = exclusion
                  ? 'out_of_scope'
                  : cell?.status ?? 'gap';
                return (
                  <td
                    key={col}
                    className={cellClass(status)}
                    title={cellTitle(cell, exclusion)}
                  >
                    {status === 'covered' && '✓'}
                    {status === 'gap' && '—'}
                    {status === 'out_of_scope' && '∅'}
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
