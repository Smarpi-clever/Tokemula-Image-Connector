export const name = 'tokemula-image';
export const inject = ['mcpManager'];

export function apply(ctx, config) {
  ctx.mcpManager.register('tokemula-image', {
    transport: 'stdio',
    command: config.command,
    args: [],
  });
}
