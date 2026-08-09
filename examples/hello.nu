[
  (mcp tool
    --input-schema {
      type: object
      properties: {name: {type: string}}
      required: [name]
    }
    hello {|args|
      $'Hello ($args.name)'
    }
  )
] | (
  mcp --transport http --port 2222
)
