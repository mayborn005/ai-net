import { MemoryRouter } from 'react-router-dom';
import type React from 'react';
import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { DashboardPanel } from './DashboardPanel';

const renderPanel = (props: Partial<React.ComponentProps<typeof DashboardPanel>> = {}) =>
  render(
    <MemoryRouter>
      <DashboardPanel
        loading={false}
        emptyTitle="No dashboard data"
        emptyDescription="Data will appear here."
        {...props}
      >
        <div>Panel content</div>
      </DashboardPanel>
    </MemoryRouter>
  );

describe('DashboardPanel', () => {
  it('renders a retryable normalized error state', () => {
    const onRetry = vi.fn();
    renderPanel({ error: new Error('Request failed'), onRetry });
    expect(screen.getByRole('alert')).toHaveTextContent('Request failed');
    screen.getByRole('button', { name: /retry/i }).click();
    expect(onRetry).toHaveBeenCalledOnce();
  });

  it('renders an explanatory empty state', () => {
    renderPanel({ empty: true });
    expect(screen.getByText('No dashboard data')).toBeInTheDocument();
    expect(screen.getByText('Data will appear here.')).toBeInTheDocument();
  });
});
