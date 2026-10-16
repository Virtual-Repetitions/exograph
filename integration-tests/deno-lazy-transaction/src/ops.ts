import type { Exograph } from "../generated/exograph.d.ts";

interface Probe {
    userCount: number
}

async function countUsers(exograph: Exograph): Promise<number> {
    const result = await exograph.executeQuery(`query { users { id } }`);
    return result.users.length;
}

async function createUser(exograph: Exograph, username: string): Promise<void> {
    await exograph.executeQuery(`
        mutation($username: String!) {
            createUser(data: { username: $username }) {
                id
            }
        }
    `, { username });
}

function sleep(ms: number): Promise<void> {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

export async function probeUserCount(exograph: Exograph): Promise<number> {
    return await countUsers(exograph);
}

export async function slowAfterContext(probe: Probe, ms: number): Promise<number> {
    await sleep(ms);
    return probe.userCount;
}

export async function readSleepWrite(probe: Probe, exograph: Exograph, username: string, ms: number): Promise<number> {
    const before = await countUsers(exograph);
    await sleep(ms);
    await createUser(exograph, username);
    return probe.userCount + before + 1;
}

export async function readWriteReadThenFail(probe: Probe, exograph: Exograph, username: string): Promise<number> {
    await countUsers(exograph);
    await createUser(exograph, username);
    // Runs inside the transaction the write opened, so it sees the uncommitted row
    const seen = await countUsers(exograph);
    throw new ExographError(`saw ${seen} users`);
}
