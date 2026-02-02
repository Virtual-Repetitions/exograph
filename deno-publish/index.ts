/*
 * Copyright Exograph, Inc.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *      https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

export type AnyVariables = Record<string, any> | undefined;

export interface Exograph {
  executeQuery<T = any>(query: string): Promise<T>;
  executeQuery<T = any, V extends AnyVariables = AnyVariables>(
    query: string,
    variables: V
  ): Promise<T>;
  addResponseHeader(name: string, value: string): Promise<void>;
  setCookie(cookie: {
    name: string,
    value: string,
    expires?: Date,
    maxAge?: number,
    domain?: string,
    path?: string,
    secure?: boolean,
    httpOnly?: boolean,
    sameSite?: "Lax" | "Strict" | "None"
  }): Promise<void>;
}

export type ContextOverride = Record<string, any> | undefined;

export interface ExographPriv extends Exograph {
  executeQueryPriv<T = any>(query: string): Promise<T>;
  executeQueryPriv<
    T = any, 
    V extends AnyVariables = AnyVariables
  >(query: string, variables: V): Promise<T>;
  executeQueryPriv<
    T = any, 
    V extends AnyVariables = AnyVariables, 
    C extends ContextOverride = ContextOverride
  >(query: string, variables: V, contextOverride: C): Promise<T>;
}

export type JsonObject = { [Key in string]?: JsonValue };
export type JsonValue = string | number | boolean | null | JsonObject | JsonValue[];

export interface Field {
  alias: string | null;
  name: string;
  arguments: JsonObject;
  subfields: Field[];
}

export interface Operation {
  name(): string;
  proceed<T>(): Promise<T>;
  query(): Field;
}

// SelectionField is the type used for computed field selection metadata
export interface SelectionField {
  name: string;
  outputName: string;
  alias?: string;
  arguments?: JsonObject;
  fields?: SelectionField[];
}

/**
 * Convert a selection field array into a GraphQL selection string.
 * 
 * @param selection - Array of selection fields
 * @param options - Configuration options
 * @returns GraphQL selection string (without wrapping braces)
 * 
 * @example
 * const selection = [
 *   { name: "id", outputName: "id" },
 *   { name: "title", outputName: "title" },
 *   { name: "author", outputName: "author", fields: [
 *     { name: "name", outputName: "name" }
 *   ]}
 * ];
 * const gql = selectionToGraphql(selection);
 * // Returns: "id title author { name }"
 */
export function selectionToGraphql(
  selection: SelectionField[],
  options?: {
    includeAliases?: boolean;
    includeArguments?: boolean;
  }
): string {
  const { includeAliases = true, includeArguments = true } = options || {};

  function fieldToGraphql(field: SelectionField, indent = ""): string {
    let result = indent;

    // Add alias if present and enabled
    if (includeAliases && field.alias && field.alias !== field.name) {
      result += `${field.alias}: `;
    }

    // Add field name
    result += field.name;

    // Add arguments if present and enabled
    if (includeArguments && field.arguments && Object.keys(field.arguments).length > 0) {
      const args = Object.entries(field.arguments)
        .map(([key, value]) => `${key}: ${JSON.stringify(value)}`)
        .join(", ");
      result += `(${args})`;
    }

    // Add nested fields if present
    if (field.fields && field.fields.length > 0) {
      result += " {\n";
      result += field.fields
        .map(f => fieldToGraphql(f, indent + "  "))
        .join("\n");
      result += `\n${indent}}`;
    }

    return result;
  }

  return selection.map(f => fieldToGraphql(f)).join("\n");
}

declare global {
  class ExographError extends Error {
    constructor(message: string);
  }
}