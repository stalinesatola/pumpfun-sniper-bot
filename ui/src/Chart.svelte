<script>
  import { onMount } from 'svelte';
  import uPlot from 'uplot';
  import 'uplot/dist/uPlot.min.css';

  let { t, v } = $props();
  let host;
  let plot;

  onMount(() => {
    const size = () => ({ width: host.clientWidth, height: host.clientHeight });
    plot = new uPlot(
      {
        ...size(),
        cursor: { drag: { x: false, y: false } },
        legend: { show: false },
        axes: [
          { stroke: '#7b8798', grid: { stroke: '#1f2733' }, ticks: { stroke: '#1f2733' } },
          { stroke: '#7b8798', grid: { stroke: '#1f2733' }, ticks: { stroke: '#1f2733' }, size: 60 },
        ],
        series: [{}, { stroke: '#5eb0ff', width: 2, fill: 'rgba(94,176,255,0.08)', points: { show: false } }],
      },
      [t, v],
      host,
    );
    const ro = new ResizeObserver(() => plot.setSize(size()));
    ro.observe(host);
    return () => {
      ro.disconnect();
      plot.destroy();
    };
  });

  // Redraw only when a point is added; the store replaces nothing else.
  $effect(() => {
    const times = $state.snapshot(t);
    const values = $state.snapshot(v);
    plot?.setData([times, values]);
  });
</script>

<div bind:this={host} class="h-full w-full"></div>
