package gateway

import (
	"context"
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/gin-gonic/gin"
	"github.com/grpc-ecosystem/grpc-gateway/v2/runtime"
	log "github.com/sirupsen/logrus"
	"google.golang.org/grpc"
	"net/http"
)

// StartGateway Starts the gateway server for the ETL component
func StartGateway() error {
	gwmux := runtime.NewServeMux()

	r := gin.Default()

	r.Any("/api/*any", gin.WrapF(gwmux.ServeHTTP))

	swagger_fs := http.FS(api.GetSwaggerEmbedded())
	r.StaticFS("/swaggerjson", swagger_fs)

	fs := http.FileSystem(http.Dir("/gateway/swagger"))

	r.GET("/", func(c *gin.Context) {
		c.Redirect(http.StatusFound, "/swagger-ui/")
	})

	conn, err := grpc.DialContext(
		context.Background(),
		"0.0.0.0:8080",
		grpc.WithBlock(),
		grpc.WithInsecure(),
	)

	r.StaticFS("/swagger-ui/", fs)

	err = api.RegisterBaktaJobsHandler(context.Background(), gwmux, conn)
	if err != nil {
		log.Println(err.Error())
		return err
	}

	return r.Run(":9000")
}
