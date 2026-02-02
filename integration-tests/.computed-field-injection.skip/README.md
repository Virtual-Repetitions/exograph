# Computed Field Injection Integration Test

This test demonstrates the new **Computed Field with Injected Exograph Client** feature.

## What This Tests

1. **Exograph Client Injection**: Computed field resolvers receive an `Exograph` client as the required 4th parameter
2. **Selection Metadata**: Resolvers can access the `SelectionField[]` to understand what was requested
3. **Selection-Aware Proxy Queries**: Using `selectionToGraphql()` helper to build GraphQL queries
4. **Policy Enforcement**: All proxy queries go through normal authorization
5. **Breaking Change**: Computed resolvers now require an injected `exograph` client

## Test Structure

```
computed-field-injection/
├── src/
│   ├── index.exo           # Schema with computed fields
│   └── resolvers.ts        # Computed field resolvers
└── tests/
    └── selection-aware-query.exotest  # Test cases
```

## Key Features Demonstrated

### 1. 4-Parameter Resolver (with Exograph injection)

```typescript
export async function fetchPublicTraining(
  parent: { uuid: string },
  args: any,
  selection: SelectionField[],
  exograph: Exograph  // ← NEW: Injected Exograph client
)
```

### 2. Selection-to-GraphQL Conversion

```typescript
const selectionText = selectionToGraphql(selection);
```

### 3. Proxy Query Execution

```typescript
const query = `
  query($uuid: Uuid!) {
    training(where: { uuid: { eq: $uuid } }) {
      ${selectionText}
    }
  }
`;
const data = await exograph.executeQuery(query, { uuid: parent.uuid });
```

## Running the Tests

```bash
# From the exograph root directory
cd integration-tests/computed-field-injection
exo test
```

## Expected Behavior

- **Test 1**: Basic selection - only `title` is fetched
- **Test 2**: Nested selection - multiple fields including `description` and `published`
- **Test 3**: Minimal selection - only `id` and `uuid`

All tests verify that:
- Only requested fields are returned (no overhydration)
- Nested selections work correctly
- The proxy query respects access policies
