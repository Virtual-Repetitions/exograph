# Computed Fields with Injected Exograph Client

This feature enables computed-field resolvers to receive an injected Exograph client and use selection-aware proxy queries.

## Overview

Computed field resolvers can now:
1. **Receive an injected Exograph client** (via 4th parameter)
2. **Access selection metadata** to understand what fields were requested
3. **Build selection-aware GraphQL queries** using the `selectionToGraphql()` helper
4. **Execute proxy queries** via `exograph.executeQuery()` with full policy enforcement

## Benefits

- **Selection fidelity**: Only fetch the fields that were actually requested
- **No overhydration**: Avoid fetching unnecessary nested data
- **Policy enforcement**: All queries go through Exograph's authorization pipeline
- **Easy maintenance**: Less custom SQL in resolvers
- **Breaking change**: Computed resolvers now require an injected `exograph` client parameter

## Resolver Signature

### 4-parameter signature (with required Exograph injection):

```typescript
export async function myComputedField(
  parent: ParentType,
  args: ArgsType,
  selection: SelectionField[],
  exograph: Exograph
): Promise<ReturnType>
```

## Example Usage

```typescript
import { selectionToGraphql } from "exograph";
import type { Exograph, SelectionField } from './generated/exograph.d.ts';

export async function fetchPublicTraining(
  parent: { uuid: string },
  args: any,
  selection: SelectionField[],
  exograph: Exograph
): Promise<any> {
  // Convert selection to GraphQL
  const selectionText = selectionToGraphql(selection);
  
  // Build a selection-aware proxy query
  const query = `
    query($uuid: Uuid!) {
      training(where: { uuid: { eq: $uuid } }) {
        ${selectionText}
      }
    }
  `;
  
  // Execute with policy enforcement
  const data = await exograph.executeQuery(query, { uuid: parent.uuid });
  
  return data.training?.[0] ?? null;
}
```

## SelectionField Type

```typescript
export interface SelectionField {
  name: string;
  outputName: string;
  alias?: string;
  arguments?: JsonObject;
  fields?: SelectionField[];  // Nested selections
}
```

## selectionToGraphql() Helper

Converts a `SelectionField[]` into a GraphQL selection string:

```typescript
function selectionToGraphql(
  selection: SelectionField[],
  options?: {
    includeAliases?: boolean;
    includeArguments?: boolean;
  }
): string
```

**Example:**

```typescript
const selection = [
  { name: "id", outputName: "id" },
  { name: "title", outputName: "title" },
  { 
    name: "author", 
    outputName: "author", 
    fields: [
      { name: "name", outputName: "name" }
    ]
  }
];

const gql = selectionToGraphql(selection);
// Returns:
// id
// title
// author {
//   name
// }
```

## Migration Guide

### Before (manual SQL hydration):

```typescript
export async function myField(
  parent: { id: number },
  args: any,
  selection: SelectionField[]
): Promise<any> {
  // Manually query database
  // Manually filter fields
  // Risk of overhydration
}
```

### After (selection-aware proxy):

```typescript
export async function myField(
  parent: { id: number },
  args: any,
  selection: SelectionField[],
  exograph: Exograph
): Promise<any> {
  const selectionText = selectionToGraphql(selection);
  const query = `query($id: Int!) { 
    myType(where: { id: { eq: $id } }) { 
      ${selectionText} 
    } 
  }`;
  
  const data = await exograph.executeQuery(query, { id: parent.id });
  return data.myType?.[0];
}
```

## Security Considerations

- All queries executed via `exograph.executeQuery()` enforce access policies
- The injected Exograph client uses the current request context
- No privilege escalation - resolvers cannot bypass authorization

## Testing

See `integration-tests/computed-field-injection/` for comprehensive test cases demonstrating:
- Basic selection-aware queries
- Nested selections
- Policy enforcement
 

## Implementation Details

- The 4th parameter is **always passed** by the runtime
- The `Exograph` shim is injected automatically
