# Recipes

A small site whose pages are templates. `templates/base.html` is the page frame;
`index.html` and `recipe.html` extend it and fill its blocks, and `partials/ingredient.html`
is included once for each ingredient. `app.vi` loads the directory with `template.load_dir`,
which checks every template at startup, and renders them with `http.render`.

Things to look at:

- Everything printed with `{{ }}` is HTML-escaped: the title `Fish pie <deluxe>` is safe.
- In `recipe.html`, the recipe goes into a `<script>` with `{{ recipe | json }}`. Without
  `| json` the template would not load: HTML escaping is not enough inside a script.
- The second recipe's source link starts with `javascript:`; the page shows `#unsafe` instead.
- `{{ recipe.summary | default("No description.") }}` covers a recipe without a summary. A
  field that does not exist at all is an error naming the template, the line and the
  expression.

## Run it

```sh
viper run main.vi          # http://localhost:8080/
DEV=1 viper run main.vi    # reload the templates when their files change
```

## Test it

```sh
viper test
```
