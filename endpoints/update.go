package endpoints

import (
	"context"
	"fmt"
	"log"

	"github.com/ag-computational-bio/bakta-web-api/go/api"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
)

//BaktaUpdateAPI implements the endpoints of the bakta-web-api
type BaktaUpdateAPI struct {
	api.UnimplementedBaktaStatusUpdateServer
	dbHandler *database.Handler
	scheduler *scheduler.SimpleScheduler
	s3Handler objectStorage.S3ObjectStorageHandler
}

//InitUpdateAPI Initializes the bakta job update API
func InitUpdateAPI() *BaktaUpdateAPI {
	return &BaktaUpdateAPI{}
}

func (apiHandler *BaktaUpdateAPI) UpdateStatus(ctx context.Context, request *api.UpdateStatusRequest) (*api.Empty, error) {
	err := apiHandler.dbHandler.CheckSecret(request.GetJobID(), request.GetSecret())
	if err != nil {
		err = fmt.Errorf("JobID does not match secret ID")
		return nil, err
	}

	err = apiHandler.dbHandler.UpdateStatus(request.GetJobID(), request.Status, request.GetError())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return &api.Empty{}, nil
}
