async (page) => {
  const sizes = [
    [320, 568],
    [360, 640],
    [375, 812],
    [390, 844],
    [412, 915],
    [430, 932],
    [768, 1024],
    [844, 390],
    [1024, 768],
  ];
  const results = [];
  for (const [width, height] of sizes) {
    await page.setViewportSize({ width, height });
    await page.waitForTimeout(80);
    results.push(await page.evaluate(({ width, height }) => {
      const composer = document.querySelector(".composer")?.getBoundingClientRect();
      const menu = document.getElementById("menuBtn")?.getBoundingClientRect();
      const sidebar = document.getElementById("sidebar")?.getBoundingClientRect();
      return {
        size: `${width}x${height}`,
        horizontalOverflow: document.documentElement.scrollWidth > width + 1,
        composerVisible: Boolean(
          composer
          && composer.top >= -1
          && composer.bottom <= height + 1
          && composer.height > 40
        ),
        composerBottom: composer ? Math.round(composer.bottom) : null,
        menuVisible: Boolean(menu && menu.width >= 40),
        sidebarInViewport: Boolean(
          sidebar
          && sidebar.left >= -1
          && sidebar.right <= width + 1
        ),
      };
    }, { width, height }));
  }
  return results;
}
