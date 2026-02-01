import { selectionToGraphql } from '../../../deno-publish/index.ts';
import type { Exograph, SelectionField } from './generated/exograph.d.ts';

/**
 * Query resolver that fetches all trainings and returns them
 * as PublicTraining objects.
 */
export async function publicTrainings(
  exograph: Exograph
): Promise<any[]> {
  const query = `
    query {
      trainings {
        uuid
        title
        description
      }
    }
  `;
  
  const data = await exograph.executeQuery(query);
  return data.trainings || [];
}

/**
 * Computed field resolver that uses the injected Exograph client
 * to fetch data with selection-aware proxy queries.
 * 
 * This demonstrates the new feature where computed resolvers can:
 * 1. Receive an injected Exograph client (4th parameter)
 * 2. Access the selection metadata (3rd parameter)
 * 3. Build a GraphQL query that mirrors the requested fields
 * 4. Execute via exograph.executeQuery() to preserve policies
 */
export async function fetchPublicTraining(
  parent: { uuid: string },
  args: any,
  selection: SelectionField[],
  exograph: Exograph
): Promise<any> {
  // Convert selection to GraphQL
  const selectionText = selectionToGraphql(selection);
  
  // Build and execute a selection-aware proxy query
  const query = `
    query($uuid: Uuid!) {
      training(where: { uuid: { eq: $uuid } }) {
        ${selectionText}
      }
    }
  `;
  
  const data = await exograph.executeQuery(query, { uuid: parent.uuid });
  
  return data.training?.[0] ?? null;
}
